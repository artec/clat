//! Own the COM apartment, retained proxies and window scope.
use super::*;
use ::windows::Win32::{
    System::Variant::VT_I4,
    System::{
        Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize, SAFEARRAY,
        },
        Ole::{
            SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement, SafeArrayGetLBound,
            SafeArrayGetUBound, SafeArrayGetVartype,
        },
    },
    UI::Accessibility::*,
};
use std::collections::HashMap;
use std::sync::Weak;

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

pub(super) struct Automation {
    pub(super) client: IUIAutomation2,
    pub(super) walker: IUIAutomationTreeWalker,
    pub(super) raw_walker: IUIAutomationTreeWalker,
    retained: HashMap<u64, Entry>,
    next: u64,
    calls: usize,
    _apartment: Apartment,
}

struct Entry {
    handle: Weak<Retained>,
    element: IUIAutomationElement,
    window: IUIAutomationElement,
    next_child: Option<(usize, IUIAutomationElement)>,
}

impl Automation {
    pub(super) fn new() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }.map_err(native)?;
        let apartment = Apartment;
        let client: IUIAutomation2 =
            unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }
                .map_err(native)?;
        unsafe { client.SetAutoSetFocus(false) }.map_err(native)?;
        let walker = unsafe { client.ControlViewWalker() }.map_err(native)?;
        let raw_walker = unsafe { client.RawViewWalker() }.map_err(native)?;
        Ok(Self {
            client,
            walker,
            raw_walker,
            retained: HashMap::new(),
            next: 0,
            calls: 0,
            _apartment: apartment,
        })
    }

    pub(super) fn begin(&mut self, call: &CancelToken) -> Result<()> {
        active(call)?;
        desktop::interactive()?;
        self.calls = self.calls.wrapping_add(1);
        if self.calls.is_multiple_of(128) {
            self.retained
                .retain(|_, entry| entry.handle.strong_count() > 0);
        }
        self.set_timeouts(call)
    }

    pub(super) fn set_timeouts(&self, call: &CancelToken) -> Result<()> {
        active(call)?;
        let ms = call
            .remaining()
            .map_or(500, |left| left.as_millis().clamp(1, 500) as u32);
        unsafe {
            self.client.SetConnectionTimeout(ms).map_err(native)?;
            self.client.SetTransactionTimeout(ms).map_err(native)
        }
    }

    fn retain(
        &mut self,
        element: IUIAutomationElement,
        window: IUIAutomationElement,
        owner: Window,
        process: &str,
        root: bool,
    ) -> Result<Handle> {
        let runtime = runtime_id(&element)?;
        let slot = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| error("NATIVE_ERROR"))?;
        let handle = Arc::new(Retained {
            key: ElementKey {
                window: owner,
                process: process.to_owned(),
                runtime,
            },
            slot,
            root,
        });
        self.retained.insert(
            slot,
            Entry {
                handle: Arc::downgrade(&handle),
                element,
                window,
                next_child: None,
            },
        );
        Ok(Arc::new(Reference::Element(handle)))
    }

    pub(super) fn windows(
        &mut self,
        app: &App,
        windows: &[Window],
        call: &CancelToken,
    ) -> Result<Vec<Handle>> {
        desktop::verify_process(app)?;
        windows
            .iter()
            .map(|&window| {
                active(call)?;
                window.check()?;
                let root =
                    unsafe { self.client.ElementFromHandle(window.hwnd()) }.map_err(native)?;
                require_pid(&root, app.pid)?;
                self.retain(root.clone(), root, window, &app.process_identity, true)
            })
            .collect()
    }

    fn entry(&self, target: &Retained) -> Result<&Entry> {
        self.retained
            .get(&target.slot)
            .ok_or_else(|| error("STALE_SNAPSHOT"))
    }

    pub(super) fn element(&self, target: &Retained) -> Result<IUIAutomationElement> {
        Ok(self.entry(target)?.element.clone())
    }

    pub(super) fn children(
        &mut self,
        target: &Retained,
        offset: usize,
        count: usize,
        call: &CancelToken,
    ) -> Result<Vec<Handle>> {
        self.check_window(target, call)?;
        let entry = self.entry(target)?;
        let (element, window) = (entry.element.clone(), entry.window.clone());
        let mut next = match &entry.next_child {
            Some((index, child)) if *index == offset => Some(child.clone()),
            _ => self.child_at(&element, offset, call)?,
        };
        let mut output = Vec::new();
        for _ in 0..count {
            let Some(child) = next.take() else { break };
            active(call)?;
            next = optional(unsafe { self.walker.GetNextSiblingElement(&child) })?;
            output.push(self.retain(
                child,
                window.clone(),
                target.key.window,
                &target.key.process,
                false,
            )?);
        }
        self.retained
            .get_mut(&target.slot)
            .expect("live entry")
            .next_child = next.map(|child| (offset + output.len(), child));
        Ok(output)
    }

    fn child_at(
        &self,
        parent: &IUIAutomationElement,
        offset: usize,
        call: &CancelToken,
    ) -> Result<Option<IUIAutomationElement>> {
        let mut next = optional(unsafe { self.walker.GetFirstChildElement(parent) })?;
        for _ in 0..offset {
            let Some(child) = next else { return Ok(None) };
            active(call)?;
            next = optional(unsafe { self.walker.GetNextSiblingElement(&child) })?;
        }
        Ok(next)
    }

    fn check_window(&self, target: &Retained, call: &CancelToken) -> Result<IUIAutomationElement> {
        active(call)?;
        target.key.window.check()?;
        let root =
            unsafe { self.client.ElementFromHandle(target.key.window.hwnd()) }.map_err(native)?;
        require_pid(&root, target.key.window.pid)?;
        if !unsafe {
            self.client
                .CompareElements(&self.entry(target)?.window, &root)
        }
        .map_err(native)?
        .as_bool()
            || runtime_id(&self.entry(target)?.window)? != runtime_id(&root)?
        {
            return Err(error("STALE_SNAPSHOT"));
        }
        Ok(root)
    }

    pub(super) fn window(&mut self, target: &Retained, call: &CancelToken) -> Result<Handle> {
        let root = self.check_window(target, call)?;
        self.verify_ancestry(target, &root, call, false)?;
        self.retain(
            root.clone(),
            root,
            target.key.window,
            &target.key.process,
            true,
        )
    }

    pub(super) fn verify_ancestry(
        &self,
        target: &Retained,
        root: &IUIAutomationElement,
        call: &CancelToken,
        require_public: bool,
    ) -> Result<bool> {
        let mut element = self.element(target)?;
        for _ in 0..=64 {
            active(call)?;
            require_pid(&element, target.key.window.pid)?;
            if require_public && elements::is_public(&element)? == Some(false) {
                return Ok(false);
            }
            if unsafe { self.client.CompareElements(&element, root) }
                .map_err(native)?
                .as_bool()
            {
                return Ok(true);
            }
            element = optional(unsafe { self.raw_walker.GetParentElement(&element) })?
                .ok_or_else(|| error("STALE_SNAPSHOT"))?;
        }
        Err(error("STALE_SNAPSHOT"))
    }

    pub(super) fn verified_element(
        &self,
        target: &Retained,
        call: &CancelToken,
    ) -> Result<IUIAutomationElement> {
        let root = self.check_window(target, call)?;
        let element = self.element(target)?;
        require_pid(&element, target.key.window.pid)?;
        if runtime_id(&element)? != target.key.runtime {
            return Err(error("STALE_SNAPSHOT"));
        }
        self.verify_ancestry(target, &root, call, false)?;
        Ok(element)
    }

    pub(super) fn public_ancestry(&self, target: &Retained, call: &CancelToken) -> Result<bool> {
        let root = self.check_window(target, call)?;
        if elements::is_public(&self.element(target)?)? != Some(true) {
            return Ok(false);
        }
        self.verify_ancestry(target, &root, call, true)
    }
}

fn require_pid(element: &IUIAutomationElement, pid: i32) -> Result<()> {
    if unsafe { element.CurrentProcessId() }.map_err(native)? != pid {
        return Err(error("FOREIGN_ELEMENT"));
    }
    Ok(())
}

pub(super) fn native(failure: ::windows::core::Error) -> ToolError {
    match failure.code().0 as u32 {
        UIA_E_ELEMENTNOTAVAILABLE => error("STALE_SNAPSHOT"),
        UIA_E_ELEMENTNOTENABLED => error("ELEMENT_DISABLED"),
        UIA_E_TIMEOUT => error("TIMEOUT"),
        0x80070005 => error("WINDOWS_TARGET_UNAVAILABLE"),
        _ => error("NATIVE_ERROR"),
    }
}

pub(super) fn optional<T>(result: ::windows::core::Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        // windows-rs represents a successful COM call with a null
        // interface (no next sibling, child, or parent) as an empty error.
        Err(failure) if failure.code().is_ok() => Ok(None),
        Err(failure) => Err(native(failure)),
    }
}

struct RuntimeId(*mut SAFEARRAY);
impl Drop for RuntimeId {
    fn drop(&mut self) {
        let _ = unsafe { SafeArrayDestroy(self.0) };
    }
}

fn runtime_id(element: &IUIAutomationElement) -> Result<Vec<i32>> {
    let id = unsafe { element.GetRuntimeId() }.map_err(native)?;
    if id.is_null() {
        return Err(error("STALE_SNAPSHOT"));
    }
    let id = RuntimeId(id);
    if unsafe { SafeArrayGetDim(id.0) } != 1
        || unsafe { SafeArrayGetVartype(id.0) }.map_err(native)? != VT_I4
    {
        return Err(error("NATIVE_ERROR"));
    }
    let low = unsafe { SafeArrayGetLBound(id.0, 1) }.map_err(native)?;
    let high = unsafe { SafeArrayGetUBound(id.0, 1) }.map_err(native)?;
    if !(0..64).contains(&(i64::from(high) - i64::from(low))) {
        return Err(error("NATIVE_ERROR"));
    }
    (low..=high)
        .map(|index| {
            let mut value = 0i32;
            unsafe { SafeArrayGetElement(id.0, &index, (&mut value as *mut i32).cast()) }
                .map_err(native)?;
            Ok(value)
        })
        .collect()
}
