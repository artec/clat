//! 百炼控制台**本地回调登录**（QUOTA-2 补债，2026-11-22）：浏览器打开
//! `bailian.console.aliyun.com/console-login`，登录完成后回调本机
//! `127.0.0.1:<port>` 交付 access_token（协议钉死于 tokenplan-quota
//! `bailian` 模块）。
use super::login_flow::{CALLBACK, LoginFailure, Start};
use super::*;
use serde_json::json;
use tokenplan_quota::{ConsoleCallbackWait, QuotaError};

/// bailian-cli 的控制台登录等待上限（15 分钟）。
const CALLBACK_DEADLINE: Duration = Duration::from_secs(15 * 60);

impl TrustedProjectApplication {
    pub fn console_bailian_login_start(&self) -> Result<Value, ApplicationError> {
        let id = match CALLBACK.start(&self.control)? {
            Start::New(id) => id,
            Start::Pending(view) => return Ok(view),
        };
        let attempt = self.begin_console_bailian_login(id);
        if attempt.is_err() {
            CALLBACK.settle(&self.control, id, Err(LoginFailure::Failed));
        }
        attempt
    }

    fn begin_console_bailian_login(&self, id: Uuid) -> Result<Value, ApplicationError> {
        // The returned URL includes the callback listener's bound port.
        let wait = ConsoleCallbackWait::start(tokenplan_quota::CONSOLE_ORIGIN_DOMESTIC).map_err(
            |error| ApplicationError::new(format!("console callback listener: {error}")),
        )?;
        let cancel = wait.cancel_handle();
        let view = json!({
            "url": wait.url,
            "expires_in": CALLBACK_DEADLINE.as_secs(),
            "interval": 1u64,
        });
        CALLBACK.publish(&self.control, id, view.clone(), Some(cancel))?;
        let control = Arc::clone(&self.control);
        let subscribers = Arc::clone(&self.subscribers);
        std::thread::Builder::new()
            .name("clat-console-callback-login".into())
            .spawn(move || {
                let result = wait_for_callback(wait);
                if CALLBACK.settle(&control, id, result) {
                    notify_changed(&subscribers);
                }
            })
            .map_err(|_| ApplicationError::new("console callback worker could not start"))?;
        Ok(view)
    }

    pub fn console_bailian_login_state(&self) -> Value {
        json!({"status": CALLBACK.status(&self.control)})
    }

    pub fn console_bailian_login_cancel(&self) {
        CALLBACK.cancel(&self.control);
    }
}

/// Receive and seal the credential. The shared flow owns durable admission.
fn wait_for_callback(wait: ConsoleCallbackWait) -> Result<ConsoleCredentialRow, LoginFailure> {
    let callback = match wait.wait(CALLBACK_DEADLINE) {
        Ok(callback) => callback,
        Err(QuotaError::Cancelled) => return Err(LoginFailure::Cancelled),
        Err(QuotaError::CallbackExpired) => return Err(LoginFailure::Expired),
        Err(_) => return Err(LoginFailure::Failed),
    };
    let blob = seal(
        &serde_json::to_string(&BailianCredential {
            access_token: callback.access_token,
            console_region: callback.console_region,
        })
        .map_err(|_| LoginFailure::Failed)?,
    )
    .map_err(|_| LoginFailure::Failed)?;
    Ok(ConsoleCredentialRow {
        client_id: None,
        blob,
        updated_at: None,
    })
}
