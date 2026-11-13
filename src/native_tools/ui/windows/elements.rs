//! UIA metadata and the three supported semantic actions.
use super::*;
use ::windows::Win32::{
    Foundation::{E_NOINTERFACE, E_POINTER},
    System::Variant::{VT_BOOL, VariantClear},
    UI::{
        Accessibility::*,
        WindowsAndMessaging::{ES_PASSWORD, GetClassNameW, GetWindowInfo, WINDOWINFO},
    },
};
use ::windows::core::Interface;
use automation::native;

const LABEL_LIMIT: usize = 384;
const VALUE_LIMIT: usize = 1024;

pub(super) fn is_public(element: &IUIAutomationElement) -> Result<Option<bool>> {
    // Ask UIA not to substitute the property's default false for an unknown value.
    let mut property = unsafe { element.GetCurrentPropertyValueEx(UIA_IsPasswordPropertyId, true) }
        .map_err(native)?;
    let result = unsafe {
        let data = &*property.Anonymous.Anonymous;
        // VARIANT_BOOL 是 i16 别名：vt 不匹配时读到的杂值被丢弃，急切求值无碍。
        (data.vt == VT_BOOL).then_some(data.Anonymous.boolVal.0 == 0)
    };
    let _ = unsafe { VariantClear(&mut property) };
    let native = native_control_privacy(element);
    if result == Some(false) || native == Some(false) {
        Ok(Some(false))
    } else {
        Ok(result.or(native))
    }
}

fn native_control_privacy(element: &IUIAutomationElement) -> Option<bool> {
    let hwnd = unsafe { element.CurrentNativeWindowHandle() }.ok()?;
    let owner = Window::from_hwnd(hwnd)?;
    if owner.pid != unsafe { element.CurrentProcessId() }.ok()? {
        return None;
    }
    let mut name = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut name) } as usize;
    let class = String::from_utf16(&name[..len]).ok()?.to_ascii_lowercase();
    match class.as_str() {
        "edit" => {
            let mut info = WINDOWINFO {
                cbSize: std::mem::size_of::<WINDOWINFO>() as u32,
                ..Default::default()
            };
            unsafe { GetWindowInfo(hwnd, &mut info) }.ok()?;
            Some(info.dwStyle.0 & ES_PASSWORD as u32 == 0)
        }
        "button" | "static" | "combobox" | "listbox" | "syslistview32" | "systreeview32" => {
            Some(true)
        }
        _ => None,
    }
}

fn pattern<T: Interface>(element: &IUIAutomationElement, id: UIA_PATTERN_ID) -> Result<Option<T>> {
    match unsafe { element.GetCurrentPatternAs(id) } {
        Ok(pattern) => Ok(Some(pattern)),
        Err(failure)
            if failure.code().is_ok()
                || matches!(failure.code(), E_POINTER | E_NOINTERFACE)
                || failure.code().0 as u32 == UIA_E_NOTSUPPORTED =>
        {
            Ok(None)
        }
        Err(failure) => Err(native(failure)),
    }
}

fn label(value: ::windows::core::Result<::windows::core::BSTR>) -> Option<String> {
    let value = value.ok()?;
    let text = String::from_utf16(&value).ok()?;
    (text.len() <= LABEL_LIMIT).then_some(text)
}

fn bounded_value(units: &[u16]) -> Option<String> {
    if units.len() > VALUE_LIMIT {
        return None;
    }
    let text = String::from_utf16(units).ok()?;
    (text.len() <= VALUE_LIMIT).then_some(text)
}

fn menu_control(role: UIA_CONTROLTYPE_ID) -> bool {
    role == UIA_MenuItemControlTypeId || role == UIA_ComboBoxControlTypeId
}

fn role_name(role: UIA_CONTROLTYPE_ID) -> String {
    // UIA control IDs 50000 through 50040 are consecutive.
    const ROLES: &[&str] = &[
        "Button",
        "Calendar",
        "CheckBox",
        "ComboBox",
        "Edit",
        "Hyperlink",
        "Image",
        "ListItem",
        "List",
        "Menu",
        "MenuBar",
        "MenuItem",
        "ProgressBar",
        "RadioButton",
        "ScrollBar",
        "Slider",
        "Spinner",
        "StatusBar",
        "Tab",
        "TabItem",
        "Text",
        "ToolBar",
        "ToolTip",
        "Tree",
        "TreeItem",
        "Custom",
        "Group",
        "Thumb",
        "DataGrid",
        "DataItem",
        "Document",
        "SplitButton",
        "Window",
        "Pane",
        "Header",
        "HeaderItem",
        "Table",
        "TitleBar",
        "Separator",
        "SemanticZoom",
        "AppBar",
    ];
    role.0
        .checked_sub(50000)
        .and_then(|id| usize::try_from(id).ok())
        .and_then(|index| ROLES.get(index))
        .map_or_else(|| "Unknown".into(), |name| (*name).into())
}

struct Patterns {
    invoke: Option<IUIAutomationInvokePattern>,
    select: Option<IUIAutomationSelectionItemPattern>,
    expand: Option<IUIAutomationExpandCollapsePattern>,
    expanded: Option<bool>,
}

impl Patterns {
    fn read(element: &IUIAutomationElement, role: UIA_CONTROLTYPE_ID) -> Result<Self> {
        let invoke = pattern(element, UIA_InvokePatternId)?;
        let select = pattern(element, UIA_SelectionItemPatternId)?;
        let mut expand: Option<IUIAutomationExpandCollapsePattern> =
            pattern(element, UIA_ExpandCollapsePatternId)?;
        let state = expand
            .as_ref()
            .map(|pattern| unsafe { pattern.CurrentExpandCollapseState() })
            .transpose()
            .map_err(native)?;
        if !menu_control(role) || state != Some(ExpandCollapseState_Collapsed) {
            expand = None;
        }
        let expanded = state
            .filter(|state| *state != ExpandCollapseState_LeafNode)
            .map(|state| state != ExpandCollapseState_Collapsed);
        Ok(Self {
            invoke,
            select,
            expand,
            expanded,
        })
    }

    fn actions(&self) -> Vec<String> {
        [
            ("press", self.invoke.is_some()),
            ("pick", self.select.is_some()),
            ("show_menu", self.expand.is_some()),
        ]
        .into_iter()
        .filter(|&(_name, present)| present)
        .map(|(name, _present)| name.to_owned())
        .collect()
    }

    fn perform(self, action: &str, call: &CancelToken, dispatch: &worker::Dispatch) -> Result<()> {
        let unsupported = || error("UNSUPPORTED_ACTION");
        let outcome = match action {
            "press" => {
                let pattern = self.invoke.ok_or_else(unsupported)?;
                dispatch.enter(call)?;
                unsafe { pattern.Invoke() }
            }
            "pick" => {
                let pattern = self.select.ok_or_else(unsupported)?;
                dispatch.enter(call)?;
                unsafe { pattern.Select() }
            }
            "show_menu" => {
                let pattern = self.expand.ok_or_else(unsupported)?;
                dispatch.enter(call)?;
                unsafe { pattern.Expand() }
            }
            _ => return Err(unsupported()),
        };
        outcome.map_err(|_| error("DISPATCH_UNKNOWN"))
    }
}

impl Automation {
    pub(super) fn describe(
        &self,
        target: &Retained,
        handle: Handle,
        call: &CancelToken,
    ) -> Result<Node> {
        let element = self.verified_element(target, call)?;
        let role = unsafe { element.CurrentControlType() }.map_err(native)?;
        let public = self.public_ancestry(target, call)?;
        let mut node = self.metadata(&element, handle, role, public, call)?;
        if !public && is_public(&element)? != Some(false) {
            node.subrole = Some("UIAUnclassified".into());
        }
        Ok(node)
    }

    fn metadata(
        &self,
        element: &IUIAutomationElement,
        handle: Handle,
        role: UIA_CONTROLTYPE_ID,
        public: bool,
        call: &CancelToken,
    ) -> Result<Node> {
        active(call)?;
        let (title, description) = if public {
            (
                label(unsafe { element.CurrentName() }),
                label(unsafe { element.CurrentHelpText() }),
            )
        } else {
            (None, None)
        };
        let enabled = unsafe { element.CurrentIsEnabled() }
            .ok()
            .map(|value| value.as_bool());
        let complete = title.is_some() && description.is_some();
        let patterns = if complete {
            Patterns::read(element, role).ok()
        } else {
            None
        };
        Ok(Node {
            handle,
            role: role_name(role),
            subrole: (!public).then(|| "UIASecure".into()),
            title: title.unwrap_or_default(),
            description,
            label_complete: complete,
            actions: patterns
                .as_ref()
                .filter(|_| enabled == Some(true))
                .map(Patterns::actions)
                .unwrap_or_default(),
            enabled,
            focused: unsafe { element.CurrentHasKeyboardFocus() }
                .ok()
                .map(|value| value.as_bool()),
            selected: patterns
                .as_ref()
                .and_then(|patterns| patterns.select.as_ref())
                .and_then(|pattern| unsafe { pattern.CurrentIsSelected() }.ok())
                .map(|value| value.as_bool()),
            expanded: patterns.and_then(|patterns| patterns.expanded),
        })
    }

    pub(super) fn value(
        &self,
        target: &Retained,
        purpose: ValuePurpose,
        call: &CancelToken,
    ) -> Result<Option<String>> {
        let element = self.verified_element(target, call)?;
        if !self.public_ancestry(target, call)? {
            return Ok(None);
        }
        let read = || -> Result<Option<String>> {
            if let Some(value) = pattern::<IUIAutomationValuePattern>(&element, UIA_ValuePatternId)?
            {
                let value = unsafe { value.CurrentValue() }.map_err(native)?;
                return Ok(bounded_value(&value));
            }
            if let Some(value) =
                pattern::<IUIAutomationRangeValuePattern>(&element, UIA_RangeValuePatternId)?
            {
                return unsafe { value.CurrentValue() }
                    .map(|number| Some(number.to_string()))
                    .map_err(native);
            }
            Ok(None)
        };
        let value = read();
        active(call)?;
        match (purpose, value) {
            (ValuePurpose::RepeatGuard, Err(failure))
                if failure
                    .recovery()
                    .is_some_and(|details| details["code"] == "NATIVE_ERROR") =>
            {
                Ok(None)
            }
            (_, result) => result,
        }
    }

    pub(super) fn dispatch(
        &self,
        target: &Retained,
        expected: Node,
        action: &str,
        call: &CancelToken,
        dispatch: &worker::Dispatch,
    ) -> Result<()> {
        let app = desktop::app(target.key.window.pid)?;
        if app.process_identity != target.key.process || app.kind == ApplicationKind::Protected {
            return Err(error("STALE_APP"));
        }
        let element = self.verified_element(target, call)?;
        let role = unsafe { element.CurrentControlType() }.map_err(native)?;
        let public = self.public_ancestry(target, call)?;
        let current = self.metadata(&element, expected.handle.clone(), role, public, call)?;
        if !current.label_complete
            || current.role != expected.role
            || current.title != expected.title
            || current.description != expected.description
            || current.subrole != expected.subrole
        {
            return Err(error("STALE_SNAPSHOT"));
        }
        if current.enabled != Some(true) {
            return Err(error("ELEMENT_DISABLED"));
        }
        if !current
            .actions
            .iter()
            .any(|advertised| advertised == action)
        {
            return Err(error("UNSUPPORTED_ACTION"));
        }
        let patterns = Patterns::read(&element, role)?;
        desktop::interactive()?;
        target.key.window.check()?;
        desktop::verify_process(&app)?;
        self.set_timeouts(call)?;
        patterns.perform(action, call, dispatch)
    }
}

#[cfg(test)]
impl Automation {
    pub(super) fn perform_fixture(
        &self,
        target: &Retained,
        action: &str,
        call: &CancelToken,
        dispatch: &worker::Dispatch,
    ) -> Result<()> {
        let element = self.verified_element(target, call)?;
        let role = unsafe { element.CurrentControlType() }.map_err(native)?;
        Patterns::read(&element, role)?.perform(action, call, dispatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cua_windows_oversize_values_do_not_match_a_truncated_prefix() {
        assert_eq!(
            bounded_value(&[b'a' as u16; VALUE_LIMIT]).unwrap().len(),
            VALUE_LIMIT
        );
        assert!(bounded_value(&[b'a' as u16; VALUE_LIMIT + 1]).is_none());
        assert!(bounded_value(&[0x4e00; VALUE_LIMIT]).is_none());
        assert!(bounded_value(&[0xd800]).is_none());
    }

    #[test]
    fn cua_windows_only_expands_menu_and_combo_box_controls() {
        assert!(menu_control(UIA_MenuItemControlTypeId));
        assert!(menu_control(UIA_ComboBoxControlTypeId));
        assert!(!menu_control(UIA_TreeItemControlTypeId));
        assert_eq!(role_name(UIA_WindowControlTypeId), "Window");
    }
}
