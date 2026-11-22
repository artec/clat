//! 百炼控制台**本地回调登录**（QUOTA-2 补债，2026-11-22）：浏览器打开
//! `bailian.console.aliyun.com/console-login`，登录完成后回调本机
//! `127.0.0.1:<port>` 交付 access_token（协议钉死于 tokenplan-quota
//! `bailian` 模块，形状镜像 device_flow 的状态机）。
use super::*;
use serde_json::json;
use tokenplan_quota::ConsoleCallbackWait;

/// bailian-cli 的控制台登录等待上限（15 分钟）。
const CALLBACK_DEADLINE: Duration = Duration::from_secs(15 * 60);

struct Flow {
    id: Uuid,
    status: &'static str,
    view: Option<Value>,
}

static FLOWS: Mutex<Option<HashMap<PathBuf, Flow>>> = Mutex::new(None);

impl TrustedProjectApplication {
    pub fn console_bailian_login_start(&self) -> Result<Value, ApplicationError> {
        let root = self.control.root_path();
        let id = Uuid::new_v4();
        {
            let mut flows = FLOWS.lock().expect("console callback flows");
            let flows = flows.get_or_insert_with(HashMap::new);
            if let Some(flow) = flows.get(root) {
                match flow.status {
                    "pending" => return Ok(flow.view.clone().expect("pending callback view")),
                    "starting" => return Err(ApplicationError::new("console login is starting")),
                    _ => {}
                }
            }
            flows.insert(
                root.to_owned(),
                Flow {
                    id,
                    status: "starting",
                    view: None,
                },
            );
        }
        // 端口必须在返回视图前就绑定好（URL 携带 127.0.0.1:port）。
        let wait = ConsoleCallbackWait::start(tokenplan_quota::CONSOLE_ORIGIN_DOMESTIC).map_err(
            |error| {
                settle(&self.control, id, Err("failed"));
                ApplicationError::new(format!("console callback listener: {error}"))
            },
        )?;
        let view = json!({
            "url": wait.url,
            "expires_in": CALLBACK_DEADLINE.as_secs(),
            "interval": 1u64,
        });
        {
            let mut flows = FLOWS.lock().expect("console callback flows");
            let flows = flows.get_or_insert_with(HashMap::new);
            let flow = current_flow(flows, &self.control, id)
                .ok_or_else(|| ApplicationError::new("console login was cancelled"))?;
            flow.status = "pending";
            flow.view = Some(view.clone());
        }
        let control = Arc::clone(&self.control);
        let subscribers = Arc::clone(&self.subscribers);
        std::thread::Builder::new()
            .name("clat-console-callback-login".into())
            .spawn(move || {
                let result = wait_for_callback(&control, wait);
                if settle(&control, id, result) {
                    notify_changed(&subscribers);
                }
            })
            .map_err(|_| ApplicationError::new("console callback worker could not start"))?;
        Ok(view)
    }

    pub fn console_bailian_login_state(&self) -> Value {
        let flows = FLOWS.lock().expect("console callback flows");
        let status = flows
            .as_ref()
            .and_then(|map| map.get(self.control.root_path()))
            .map_or("none", |flow| flow.status);
        json!({"status": status})
    }

    pub fn console_bailian_login_cancel(&self) {
        let mut flows = FLOWS.lock().expect("console callback flows");
        if let Some(flows) = flows.as_mut()
            && flows
                .get(self.control.root_path())
                .is_some_and(|flow| matches!(flow.status, "starting" | "pending"))
        {
            flows.remove(self.control.root_path());
        }
    }
}

fn wait_for_callback(
    control: &ControlStorage,
    wait: ConsoleCallbackWait,
) -> Result<(), &'static str> {
    let callback = match wait.wait(CALLBACK_DEADLINE) {
        Ok(callback) => callback,
        Err(error) => {
            let expired = error.to_string().contains("timed out");
            return Err(if expired { "expired" } else { "failed" });
        }
    };
    let blob = seal(
        &serde_json::to_string(&BailianCredential {
            access_token: callback.access_token,
            console_region: callback.console_region,
        })
        .map_err(|_| "failed")?,
    )
    .map_err(|_| "failed")?;
    control
        .write_console_credential(
            "bailian",
            &ConsoleCredentialRow {
                client_id: None,
                blob,
                updated_at: None,
            },
        )
        .map_err(|_| "failed")?;
    invalidate_cache(control, "bailian");
    Ok(())
}

fn current_flow<'a>(
    flows: &'a mut HashMap<PathBuf, Flow>,
    control: &ControlStorage,
    id: Uuid,
) -> Option<&'a mut Flow> {
    flows
        .get_mut(control.root_path())
        .filter(|flow| flow.id == id)
}

fn settle(control: &ControlStorage, id: Uuid, result: Result<(), &'static str>) -> bool {
    let mut flows = FLOWS.lock().expect("console callback flows");
    let Some(flows) = flows.as_mut() else {
        return false;
    };
    let Some(flow) = current_flow(flows, control, id) else {
        return false;
    };
    if matches!(flow.status, "starting" | "pending") {
        flow.status = match result {
            Ok(()) => "complete",
            Err("expired") => "expired",
            Err(_) => "failed",
        };
        true
    } else {
        false
    }
}

pub(super) fn remove(control: &ControlStorage) -> Result<(), ApplicationError> {
    let mut flows = FLOWS.lock().expect("console callback flows");
    remove_credential(control, "bailian")?;
    if let Some(flows) = flows.as_mut() {
        flows.remove(control.root_path());
    }
    Ok(())
}
