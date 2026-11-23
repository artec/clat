//! 百炼控制台**本地回调登录**（QUOTA-2 补债，2026-11-22）：浏览器打开
//! `bailian.console.aliyun.com/console-login`，登录完成后回调本机
//! `127.0.0.1:<port>` 交付 access_token（协议钉死于 tokenplan-quota
//! `bailian` 模块，形状镜像 device_flow 的状态机）。
use super::*;
use serde_json::json;
use tokenplan_quota::{ConsoleCallbackCancel, ConsoleCallbackWait, QuotaError};

/// bailian-cli 的控制台登录等待上限（15 分钟）。
const CALLBACK_DEADLINE: Duration = Duration::from_secs(15 * 60);

struct Flow {
    id: Uuid,
    status: &'static str,
    view: Option<Value>,
    /// Ends the background wait when the flow is cancelled or removed.
    cancel: Option<ConsoleCallbackCancel>,
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
                    cancel: None,
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
        let cancel = wait.cancel_handle();
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
            flow.cancel = Some(cancel);
        }
        let control = Arc::clone(&self.control);
        let subscribers = Arc::clone(&self.subscribers);
        std::thread::Builder::new()
            .name("clat-console-callback-login".into())
            .spawn(move || {
                let result = wait_for_callback(wait);
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

    /// 取消是撤销，不是“忘掉它”：除了删表项，还要唤醒后台等待、关掉监听
    /// 器。迟到的回调由 [`settle`] 的当前流校验挡在落盘之前。
    pub fn console_bailian_login_cancel(&self) {
        let mut flows = FLOWS.lock().expect("console callback flows");
        if let Some(flows) = flows.as_mut()
            && flows
                .get(self.control.root_path())
                .is_some_and(|flow| matches!(flow.status, "starting" | "pending"))
        {
            cancel_flow(flows.remove(self.control.root_path()));
        }
    }
}

/// 等到回调并封存它交付的内容。**不落盘**：凭据只在登录仍是当前流时
/// 才值得保存，这一判断与写入同在 [`settle`] 里持锁完成。
fn wait_for_callback(wait: ConsoleCallbackWait) -> Result<ConsoleCredentialRow, &'static str> {
    let callback = match wait.wait(CALLBACK_DEADLINE) {
        Ok(callback) => callback,
        Err(QuotaError::Cancelled) => return Err("cancelled"),
        Err(error) if error.to_string().contains("timed out") => return Err("expired"),
        Err(_) => return Err("failed"),
    };
    let blob = seal(
        &serde_json::to_string(&BailianCredential {
            access_token: callback.access_token,
            console_region: callback.console_region,
        })
        .map_err(|_| "failed")?,
    )
    .map_err(|_| "failed")?;
    Ok(ConsoleCredentialRow {
        client_id: None,
        blob,
        updated_at: None,
    })
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

/// 记录一次等待的结局，也是**唯一**写入凭据的地方：先在 `FLOWS` 锁内
/// 确认该流仍是当前流，再落盘。取消、移除凭据和新发起的登录都取同一把
/// 锁，所以迟到的回调要么先于它们完成（登录确实成功），要么发现流已
/// 不在而什么都不写（2026-11-23 审计 A-1）。
fn settle(
    control: &ControlStorage,
    id: Uuid,
    result: Result<ConsoleCredentialRow, &'static str>,
) -> bool {
    let mut flows = FLOWS.lock().expect("console callback flows");
    let Some(flow) = flows
        .as_mut()
        .and_then(|flows| current_flow(flows, control, id))
    else {
        return false;
    };
    if !matches!(flow.status, "starting" | "pending") {
        return false;
    }
    flow.status = match result {
        Ok(row) if control.write_console_credential("bailian", &row).is_ok() => "complete",
        Ok(_) => "failed",
        Err("expired") => "expired",
        Err(_) => "failed",
    };
    let stored = flow.status == "complete";
    drop(flows);
    if stored {
        invalidate_cache(control, "bailian");
    }
    true
}

pub(super) fn remove(control: &ControlStorage) -> Result<(), ApplicationError> {
    let mut flows = FLOWS.lock().expect("console callback flows");
    remove_credential(control, "bailian")?;
    // 移除凭据同样撤销在途登录：同一把锁加上 `settle` 的当前流校验，
    // 让迟到的回调无法把刚删掉的凭据写回来。
    cancel_flow(
        flows
            .as_mut()
            .and_then(|flows| flows.remove(control.root_path())),
    );
    Ok(())
}

/// 撤销一个流：先置取消标志，后台等待随即返回并丢弃监听器。
fn cancel_flow(flow: Option<Flow>) {
    if let Some(cancel) = flow.and_then(|flow| flow.cancel) {
        cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 判别（2026-11-23 审计 A-1）：只有仍是当前流的等待结局才能写凭据。
    /// 取消后、被新登录取代后到达的回调都必须落空。
    #[test]
    fn settle_stores_the_credential_only_for_the_current_flow() {
        let root = std::env::temp_dir().join(format!(
            "clat-bailian-settle-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let control = ControlStorage::open_ready(&root).unwrap();
        let sealed = || ConsoleCredentialRow {
            client_id: None,
            blob: seal("delivered").unwrap(),
            updated_at: None,
        };

        // A cancelled login: the flow is gone before its callback lands.
        let cancelled = Uuid::new_v4();
        {
            let mut flows = FLOWS.lock().unwrap();
            flows
                .get_or_insert_with(HashMap::new)
                .insert(control.root_path().to_path_buf(), pending(cancelled));
            cancel_flow(
                flows
                    .as_mut()
                    .and_then(|flows| flows.remove(control.root_path())),
            );
        }
        assert!(!settle(&control, cancelled, Ok(sealed())));
        assert_eq!(control.read_console_credential("bailian").unwrap(), None);

        // A superseded login: a newer flow owns the slot when the old wait ends.
        let (old, current) = (Uuid::new_v4(), Uuid::new_v4());
        FLOWS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(control.root_path().to_path_buf(), pending(current));
        assert!(!settle(&control, old, Ok(sealed())));
        assert_eq!(control.read_console_credential("bailian").unwrap(), None);
        assert_eq!(
            FLOWS.lock().unwrap().as_ref().unwrap()[control.root_path()].status,
            "pending",
            "a superseded wait must not settle the newer flow either"
        );

        // The current flow stores the credential and completes.
        assert!(settle(&control, current, Ok(sealed())));
        assert!(
            control
                .read_console_credential("bailian")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            FLOWS.lock().unwrap().as_ref().unwrap()[control.root_path()].status,
            "complete"
        );
        // A terminal flow is not settled a second time.
        assert!(!settle(&control, current, Err("expired")));

        FLOWS
            .lock()
            .unwrap()
            .as_mut()
            .map(|flows| flows.remove(control.root_path()));
        drop(control);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn pending(id: Uuid) -> Flow {
        Flow {
            id,
            status: "pending",
            view: None,
            cancel: None,
        }
    }
}
