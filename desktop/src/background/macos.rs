//! Same WindowServer blur-radius API used by iTerm2. Resolve it dynamically:
//! unsupported future systems retain the standard Tauri material fallback.
use clat_core::client_ports::presentation::UiMessage;
use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

type Connection = unsafe extern "C" fn() -> u32;
type Blur = unsafe extern "C" fn(u32, u32, i32) -> i32;
#[derive(Clone, Copy)]
struct Api {
    connection: Connection,
    blur: Blur,
}
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
fn api() -> Option<Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    *API.get_or_init(|| unsafe {
        // Keep the framework loaded for the lifetime of the cached function pointers.
        let handle = dlopen(
            c"/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics".as_ptr(),
            1,
        );
        if handle.is_null() {
            return None;
        }
        let connection = dlsym(handle, c"CGSMainConnectionID".as_ptr());
        let blur = dlsym(handle, c"CGSSetWindowBackgroundBlurRadius".as_ptr());
        if connection.is_null() || blur.is_null() {
            return None;
        }
        Some(Api {
            connection: std::mem::transmute::<*mut c_void, Connection>(connection),
            blur: std::mem::transmute::<*mut c_void, Blur>(blur),
        })
    })
}
pub fn available() -> bool {
    api().is_some()
}

pub async fn apply(window: &tauri::WebviewWindow, strength: u8) -> Result<(), UiMessage> {
    let Some(api) = api() else {
        return Ok(());
    };
    let (send, receive) = tokio::sync::oneshot::channel();
    let native = window.clone();
    window
        .run_on_main_thread(move || {
            let result = (|| -> Result<(), UiMessage> {
                let pointer = native.ns_window().map_err(|e| e.to_string())?;
                // Tauri owns this NSWindow; only borrow it within the main-thread closure.
                let number: isize = unsafe {
                    objc2::msg_send![&*pointer.cast::<objc2::runtime::AnyObject>(), windowNumber]
                };
                let connection = unsafe { (api.connection)() };
                let code = unsafe {
                    (api.blur)(
                        connection,
                        number as u32,
                        (u32::from(strength) * 64).div_ceil(100) as i32,
                    )
                };
                if code == 0 {
                    Ok(())
                } else {
                    Err(UiMessage::new("desktop.background_blur_failed")
                        .arg("code", code.to_string()))
                }
            })();
            let _ = send.send(result);
        })
        .map_err(|e| e.to_string())?;
    receive.await.map_err(|e| e.to_string())?
}
