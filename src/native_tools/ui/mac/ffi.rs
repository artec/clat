//! Ownership and typed access for the public Core Foundation / AX C APIs.
use super::*;
use std::ffi::{CStr, c_char, c_void};
use std::ptr::{NonNull, null, null_mut};

pub(super) type Raw = *const c_void;
pub(super) const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    unsafe fn CFRelease(value: Raw);
    unsafe fn CFRetain(value: Raw) -> Raw;
    unsafe fn CFEqual(left: Raw, right: Raw) -> u8;
    unsafe fn CFHash(value: Raw) -> usize;
    unsafe fn CFGetTypeID(value: Raw) -> usize;
    unsafe fn CFStringGetTypeID() -> usize;
    unsafe fn CFNullGetTypeID() -> usize;
    unsafe fn CFBooleanGetTypeID() -> usize;
    unsafe fn CFNumberGetTypeID() -> usize;
    unsafe fn CFArrayGetTypeID() -> usize;
    unsafe fn CFStringCreateWithCString(allocator: Raw, value: *const c_char, encoding: u32)
    -> Raw;
    unsafe fn CFStringGetLength(value: Raw) -> isize;
    unsafe fn CFStringGetBytes(
        value: Raw,
        range: CfRange,
        encoding: u32,
        loss: u8,
        external: u8,
        buffer: *mut u8,
        size: isize,
        used: *mut isize,
    ) -> isize;
    unsafe fn CFBooleanGetValue(value: Raw) -> u8;
    unsafe fn CFNumberGetValue(value: Raw, kind: isize, out: *mut c_void) -> u8;
    unsafe fn CFArrayCreate(
        allocator: Raw,
        values: *const Raw,
        count: isize,
        callbacks: Raw,
    ) -> Raw;
    unsafe fn CFArrayGetCount(value: Raw) -> isize;
    unsafe fn CFArrayGetValueAtIndex(value: Raw, index: isize) -> Raw;
    unsafe fn CFDictionaryCreate(
        allocator: Raw,
        keys: *const Raw,
        values: *const Raw,
        count: isize,
        key_callbacks: Raw,
        value_callbacks: Raw,
    ) -> Raw;
    unsafe fn CFDictionaryGetValue(dictionary: Raw, key: Raw) -> Raw;
    static kCFBooleanTrue: Raw;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    pub(super) unsafe fn AXIsProcessTrusted() -> u8;
    unsafe fn AXIsProcessTrustedWithOptions(options: Raw) -> u8;
    static kAXTrustedCheckOptionPrompt: Raw;
    pub(super) unsafe fn AXUIElementCreateApplication(pid: i32) -> Raw;
    unsafe fn AXUIElementGetTypeID() -> usize;
    unsafe fn AXUIElementGetPid(node: Raw, pid: *mut i32) -> i32;
    unsafe fn AXUIElementSetMessagingTimeout(node: Raw, timeout: f32) -> i32;
    unsafe fn AXUIElementCopyAttributeValue(node: Raw, name: Raw, value: *mut Raw) -> i32;
    unsafe fn AXUIElementGetAttributeValueCount(node: Raw, name: Raw, count: *mut isize) -> i32;
    unsafe fn AXUIElementCopyAttributeValues(
        node: Raw,
        name: Raw,
        offset: isize,
        count: isize,
        values: *mut Raw,
    ) -> i32;
    unsafe fn AXUIElementCopyMultipleAttributeValues(
        node: Raw,
        names: Raw,
        options: u32,
        values: *mut Raw,
    ) -> i32;
    unsafe fn AXUIElementCopyActionNames(node: Raw, names: *mut Raw) -> i32;
    unsafe fn AXUIElementPerformAction(node: Raw, action: Raw) -> i32;
    unsafe fn AXValueGetTypeID() -> usize;
    unsafe fn AXValueGetValue(value: Raw, kind: u32, out: *mut c_void) -> u8;
}

pub(in crate::native_tools::ui) fn prompt_accessibility() -> Result<bool> {
    // The dictionary borrows process-lifetime constants.
    let options = Cf::owned(unsafe {
        CFDictionaryCreate(
            null(),
            &kAXTrustedCheckOptionPrompt,
            &kCFBooleanTrue,
            1,
            null(),
            null(),
        )
    })
    .ok_or_else(|| error("NATIVE_ERROR"))?;
    Ok(unsafe { AXIsProcessTrustedWithOptions(options.raw()) != 0 })
}

#[repr(C)]
struct CfRange {
    location: isize,
    length: isize,
}

pub(super) struct Cf(NonNull<c_void>);

// CF values acquired here are retained immutable objects. The per-reference
// lock prevents simultaneous changes to an AX reference's messaging timeout.
pub(super) struct Ax {
    pub(super) value: Cf,
    ipc: Mutex<()>,
}

// SAFETY: CF retains/releases and reads of immutable objects are thread-safe.
// AX IPC and timeout changes are serialized in Ax::ipc.
unsafe impl Send for Cf {}
unsafe impl Sync for Cf {}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.raw()) }
    }
}

impl Cf {
    pub(super) fn owned(raw: Raw) -> Option<Self> {
        NonNull::new(raw.cast_mut()).map(Self)
    }

    fn retained(raw: Raw) -> Option<Self> {
        if raw.is_null() {
            return None;
        }
        Self::owned(unsafe { CFRetain(raw) })
    }

    pub(super) fn raw(&self) -> Raw {
        self.0.as_ptr()
    }

    pub(super) fn key(value: &CStr) -> Result<Self> {
        Self::owned(unsafe { CFStringCreateWithCString(null(), value.as_ptr(), UTF8) })
            .ok_or_else(|| error("NATIVE_ERROR"))
    }

    pub(super) fn hash(&self) -> u64 {
        unsafe { CFHash(self.raw()) as u64 }
    }

    pub(super) fn same(&self, other: &Self) -> bool {
        unsafe { CFEqual(self.raw(), other.raw()) != 0 }
    }

    pub(super) fn string<const LIMIT: usize>(&self) -> Option<String> {
        if unsafe { CFGetTypeID(self.raw()) != CFStringGetTypeID() } {
            return None;
        }
        let length = unsafe { CFStringGetLength(self.raw()) };
        if length > LIMIT as isize {
            return None;
        }
        let mut buffer = [0_u8; LIMIT];
        let mut used = 0;
        let range = CfRange {
            location: 0,
            length,
        };
        let copied = unsafe {
            CFStringGetBytes(
                self.raw(),
                range,
                UTF8,
                0,
                0,
                buffer.as_mut_ptr(),
                LIMIT as isize,
                &mut used,
            )
        };
        (copied == length)
            .then(|| {
                std::str::from_utf8(&buffer[..used as usize])
                    .ok()
                    .map(str::to_owned)
            })
            .flatten()
    }

    pub(super) fn boolean(&self) -> Option<bool> {
        (unsafe { CFGetTypeID(self.raw()) == CFBooleanGetTypeID() })
            .then(|| unsafe { CFBooleanGetValue(self.raw()) != 0 })
    }

    pub(super) fn scalar(&self) -> Option<String> {
        if let Some(value) = self.boolean() {
            return Some(value.to_string());
        }
        if unsafe { CFGetTypeID(self.raw()) == CFNumberGetTypeID() } {
            let mut value = 0.0_f64;
            let copied =
                unsafe { CFNumberGetValue(self.raw(), 6, (&mut value as *mut f64).cast()) };
            return (copied != 0 && value.is_finite()).then(|| value.to_string());
        }
        self.string::<1025>()
    }

    pub(super) fn dict_flag(&self, key: &CStr) -> Option<bool> {
        let key = Self::key(key).ok()?;
        Self::retained(unsafe { CFDictionaryGetValue(self.raw(), key.raw()) })?.boolean()
    }

    fn list(&self, limit: usize) -> Result<Vec<Self>> {
        if unsafe { CFGetTypeID(self.raw()) != CFArrayGetTypeID() } {
            return Err(error("NATIVE_ERROR"));
        }
        let count = unsafe { CFArrayGetCount(self.raw()) }.max(0) as usize;
        if count > limit {
            return Err(error("NATIVE_ERROR"));
        }
        (0..count)
            .map(|index| {
                Self::retained(unsafe { CFArrayGetValueAtIndex(self.raw(), index as isize) })
                    .ok_or_else(|| error("NATIVE_ERROR"))
            })
            .collect()
    }

    fn ax_error(&self) -> Option<i32> {
        if unsafe { CFGetTypeID(self.raw()) != AXValueGetTypeID() } {
            return None;
        }
        let mut result = 0_i32;
        let copied = unsafe { AXValueGetValue(self.raw(), 5, (&mut result as *mut i32).cast()) };
        (copied != 0).then_some(result)
    }

    fn handle(self) -> Result<Handle> {
        if unsafe { CFGetTypeID(self.raw()) != AXUIElementGetTypeID() } {
            return Err(error("NATIVE_ERROR"));
        }
        Ok(Arc::new(Ax {
            value: self,
            ipc: Mutex::new(()),
        }))
    }
}

pub(super) fn ax(handle: &Handle) -> &Ax {
    handle.downcast_ref().expect("macOS accessibility handle")
}

pub(super) fn application(pid: i32) -> Result<Handle> {
    Cf::owned(unsafe { AXUIElementCreateApplication(pid) })
        .ok_or_else(|| error("STALE_APP"))?
        .handle()
}

impl Ax {
    fn call<T>(&self, cancel: &CancelToken, call: impl FnOnce(Raw) -> Result<T>) -> Result<T> {
        active(cancel)?;
        let _guard = self.ipc.try_lock().map_err(|_| error("NATIVE_BUSY"))?;
        let timeout = cancel
            .remaining()
            .unwrap_or(Duration::from_millis(250))
            .min(Duration::from_millis(250))
            .as_secs_f32();
        if timeout == 0.0 {
            return Err(error("TIMEOUT"));
        }
        let node = self.value.raw();
        status(
            unsafe { AXUIElementSetMessagingTimeout(node, timeout) },
            cancel,
        )?;
        active(cancel)?;
        call(node)
    }

    pub(super) fn pid(&self) -> Result<i32> {
        let mut pid = 0;
        if unsafe { AXUIElementGetPid(self.value.raw(), &mut pid) } != 0 || pid <= 0 {
            return Err(error("STALE_SNAPSHOT"));
        }
        Ok(pid)
    }

    pub(super) fn attr(&self, name: &CStr, cancel: &CancelToken) -> Result<Option<Cf>> {
        let name = Cf::key(name)?;
        self.call(cancel, |node| {
            copy_optional(cancel, |out| unsafe {
                AXUIElementCopyAttributeValue(node, name.raw(), out)
            })
        })
    }

    pub(super) fn element(&self, name: &CStr, cancel: &CancelToken) -> Result<Option<Handle>> {
        self.attr(name, cancel)?.map(Cf::handle).transpose()
    }

    pub(super) fn attrs<const N: usize>(
        &self,
        names: [&CStr; N],
        cancel: &CancelToken,
    ) -> Result<Vec<Option<Cf>>> {
        let keys = names
            .iter()
            .map(|name| Cf::key(name))
            .collect::<Result<Vec<_>>>()?;
        let raw = keys.iter().map(Cf::raw).collect::<Vec<_>>();
        // The array borrows the keys, all retained until after the AX copy.
        let array = Cf::owned(unsafe { CFArrayCreate(null(), raw.as_ptr(), N as isize, null()) })
            .ok_or_else(|| error("NATIVE_ERROR"))?;
        let values = self.call(cancel, |node| {
            copy_optional(cancel, |out| unsafe {
                AXUIElementCopyMultipleAttributeValues(node, array.raw(), 0, out)
            })
        })?;
        let Some(values) = values else {
            return names
                .into_iter()
                .map(|name| self.attr(name, cancel))
                .collect();
        };
        let values = values.list(N)?;
        if values.len() != N {
            return Err(error("NATIVE_ERROR"));
        }
        values
            .into_iter()
            .map(|value| {
                if unsafe { CFGetTypeID(value.raw()) == CFNullGetTypeID() } {
                    return Ok(None);
                }
                match value.ax_error() {
                    Some(code) if absent(code) => Ok(None),
                    Some(code) => status(code, cancel).map(|_| None),
                    None => Ok(Some(value)),
                }
            })
            .collect()
    }

    pub(super) fn count(&self, key: &CStr, cancel: &CancelToken) -> Result<Option<usize>> {
        self.count_with_key(&Cf::key(key)?, cancel)
    }

    fn count_with_key(&self, key: &Cf, cancel: &CancelToken) -> Result<Option<usize>> {
        let mut size = 0;
        self.call(cancel, |node| {
            match unsafe { AXUIElementGetAttributeValueCount(node, key.raw(), &mut size) } {
                code if absent(code) => Ok(None),
                code => {
                    status(code, cancel)?;
                    Ok(Some(size.max(0) as usize))
                }
            }
        })
    }

    pub(super) fn children(
        &self,
        key: &CStr,
        offset: usize,
        count: usize,
        cancel: &CancelToken,
    ) -> Result<Vec<Handle>> {
        let key = Cf::key(key)?;
        let offset = isize::try_from(offset).map_err(|_| error("INVALID_ARGUMENTS"))?;
        let size = self.count_with_key(&key, cancel)?.unwrap_or(0) as isize;
        if offset >= size {
            return Ok(Vec::new());
        }
        let count = count.min((size - offset) as usize);
        let children = self.call(cancel, |node| {
            copy_optional(cancel, |out| unsafe {
                AXUIElementCopyAttributeValues(node, key.raw(), offset, count as isize, out)
            })
        })?;
        children.map_or_else(
            || Ok(Vec::new()),
            |value| value.list(count)?.into_iter().map(Cf::handle).collect(),
        )
    }

    pub(super) fn actions(&self, cancel: &CancelToken) -> Result<Vec<String>> {
        let actions = self.call(cancel, |node| {
            copy_optional(cancel, |out| unsafe {
                AXUIElementCopyActionNames(node, out)
            })
        })?;
        actions.map_or_else(
            || Ok(Vec::new()),
            |value| {
                Ok(value
                    .list(64)?
                    .iter()
                    .filter_map(Cf::string::<65>)
                    .collect())
            },
        )
    }

    pub(super) fn perform(&self, action: &CStr, cancel: &CancelToken) -> Result<()> {
        let action = Cf::key(action)?;
        self.call(cancel, |node| {
            match unsafe { AXUIElementPerformAction(node, action.raw()) } {
                0 => Ok(()),
                -25202 => Err(error("STALE_SNAPSHOT")),
                -25206 | -25208 => Err(error("UNSUPPORTED_ACTION")),
                -25211 => Err(error("PERMISSION_DENIED")),
                _ => Err(error("DISPATCH_UNKNOWN")),
            }
        })
    }
}

fn copy_optional(cancel: &CancelToken, copy: impl FnOnce(*mut Raw) -> i32) -> Result<Option<Cf>> {
    let mut raw = null_mut::<c_void>().cast_const();
    let code = copy(&mut raw);
    let value = Cf::owned(raw);
    if absent(code) {
        return Ok(None);
    }
    status(code, cancel)?;
    Ok(value)
}

fn absent(code: i32) -> bool {
    matches!(code, -25205 | -25206 | -25208 | -25212 | -25213)
}

fn status(code: i32, cancel: &CancelToken) -> Result<()> {
    if code == 0 {
        return Ok(());
    }
    active(cancel)?;
    Err(error(match code {
        -25201 | -25202 => "STALE_SNAPSHOT",
        -25204 => "NATIVE_BUSY",
        -25211 => "PERMISSION_DENIED",
        _ => "NATIVE_ERROR",
    }))
}
