use super::*;

const ACTIONS: &[&str] = &[
    "stage",
    "unstage",
    "discard",
    "commit",
    "generate",
    "branch.create",
    "branch.switch",
    "branch.delete",
    "fetch",
    "pull",
    "push",
    "restore",
];

impl GitService {
    pub(super) fn start(
        self: &Arc<Self>,
        request: Value,
        model: Option<GitModel>,
        settings_owner: &Self,
    ) -> Result<Value, String> {
        let id = identity(&request, "operationId")?;
        let signature = digest(request.to_string().as_bytes());
        let mut operations = self.operations.lock().map_err(|_| "git_busy")?;
        if let Some(op) = operations.get(&id) {
            let saved = op.saved.lock().map_err(|_| "git_busy")?;
            return if saved.signature == signature {
                Ok(json!(saved.view))
            } else {
                Err("operation_conflict".into())
            };
        }
        let persisted: SavedOperations = self.store.load("git-operations.json")?;
        if let Some(saved) = persisted.rows.get(&id) {
            return if saved.signature == signature {
                Ok(json!(self.recover_view(saved)))
            } else {
                Err("operation_conflict".into())
            };
        }
        if self.closed.load(Ordering::SeqCst) {
            return Err("host_closed".into());
        }
        let action = text(&request, "action")?;
        if !ACTIONS.contains(&action) {
            return Err("invalid_action".into());
        }
        if operations
            .values()
            .filter(|op| {
                matches!(
                    op.saved.lock().expect("git operation").view.phase.as_str(),
                    "pending" | "running" | "generating" | "committing"
                )
            })
            .count()
            >= 4
        {
            return Err("git_busy".into());
        }
        let settings = if matches!(action, "commit" | "generate") {
            settings_owner.preferences()?
        } else {
            GitSettings::default()
        };
        let op = Arc::new(Operation {
            saved: Mutex::new(SavedOperation {
                signature,
                push_after_commit: action == "commit" && settings.auto_push,
                request: compact_request(&request),
                view: OperationView::pending(&id),
            }),
            cancel: CancelToken::new(),
        });
        operations.insert(id.clone(), op.clone());
        drop(operations);
        if let Err(error) = self.persist() {
            self.operations.lock().map_err(|_| "git_busy")?.remove(&id);
            return Err(error);
        }
        self.spawn_operation(op, request, model, settings)?;
        self.operation(&json!({"operationId":id}), false)
    }
    fn spawn_operation(
        self: &Arc<Self>,
        op: Arc<Operation>,
        request: Value,
        model: Option<GitModel>,
        settings: GitSettings,
    ) -> Result<(), String> {
        let producer = self.store.lifetime.producer()?;
        let service = self.clone();
        let worker_op = op.clone();
        match std::thread::Builder::new()
            .name("clat-human-git".into())
            .spawn(move || {
                let _producer = producer;
                service.run_operation(&worker_op, &request, model, settings)
            }) {
            Ok(thread) => {
                self.workers.lock().map_err(|_| "git_busy")?.push(thread);
                Ok(())
            }
            Err(error) => {
                let reason = io(error);
                {
                    let mut saved = op.saved.lock().map_err(|_| "git_busy")?;
                    saved.view.phase = "failed".into();
                    saved.view.presentation = Some(errors::presentation(&reason));
                    saved.view.error = Some(reason.clone());
                }
                self.persist()?;
                Err(reason)
            }
        }
    }
    fn run_operation(
        &self,
        op: &Operation,
        request: &Value,
        model: Option<GitModel>,
        settings: GitSettings,
    ) {
        let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.execute(op, request, model, &settings)
        }))
        .unwrap_or_else(|_| Err("operation_failed".into()));
        if result.is_err() {
            let saved = op.saved.lock().expect("git operation").clone();
            if saved.view.phase == "committing" && self.commit_was_written(&saved) {
                result = Ok(reconciled_commit(&saved));
            }
        }
        {
            let mut saved = op.saved.lock().expect("git operation");
            match result {
                Ok(value) => {
                    saved.view.phase = "completed".into();
                    saved.view.result = Some(compact_result(value));
                }
                Err(error) => {
                    saved.view.phase = if op.cancel.is_cancelled() {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                    saved.view.presentation = Some(errors::presentation(&error));
                    saved.view.error = Some(error);
                }
            }
        }
        let _ = self.persist();
    }
    pub(super) fn operation(&self, value: &Value, cancel: bool) -> Result<Value, String> {
        let id = identity(value, "operationId")?;
        if let Some(op) = self
            .operations
            .lock()
            .map_err(|_| "git_busy")?
            .get(&id)
            .cloned()
        {
            if cancel {
                op.cancel.cancel();
            }
            return Ok(json!(op.saved.lock().map_err(|_| "git_busy")?.view));
        }
        let rows: SavedOperations = self.store.load("git-operations.json")?;
        let saved = rows.rows.get(&id).ok_or("operation_missing")?;
        Ok(json!(self.recover_view(saved)))
    }
    fn recover_view(&self, saved: &SavedOperation) -> OperationView {
        let mut view = saved.view.clone();
        if matches!(
            view.phase.as_str(),
            "pending" | "running" | "generating" | "committing"
        ) {
            view.phase = "unknown".into();
            view.error = Some("operation_unknown".into());
            view.presentation = Some(errors::presentation("operation_unknown"));
            if saved.request["action"] == "commit" && self.commit_was_written(saved) {
                view.phase = "completed".into();
                view.error = None;
                view.presentation = None;
                view.result = Some(reconciled_commit(saved));
            }
        }
        view
    }
    fn commit_was_written(&self, saved: &SavedOperation) -> bool {
        saved
            .view
            .result
            .as_ref()
            .is_some_and(|result| result["push"]["status"] == "unknown")
            || (saved.view.message.is_some() && self.commit_matches(saved))
    }
    pub(super) fn phase(
        &self,
        op: &Operation,
        phase: &str,
        message: Option<String>,
    ) -> Result<(), String> {
        {
            let mut saved = op.saved.lock().map_err(|_| "git_busy")?;
            saved.view.phase = phase.into();
            if message.is_some() {
                saved.view.message = message;
            }
        }
        self.persist()
    }
    pub(super) fn persist(&self) -> Result<(), String> {
        let operations = self.operations.lock().map_err(|_| "git_busy")?;
        let mut saved: SavedOperations = self.store.load("git-operations.json")?;
        for (id, op) in operations.iter() {
            saved
                .rows
                .insert(id.clone(), op.saved.lock().map_err(|_| "git_busy")?.clone());
        }
        self.store.save("git-operations.json", &saved)
    }
}

fn compact_request(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(expected) = value["expected"].as_object_mut() {
        expected.remove("files");
    }
    value
}
fn compact_result(mut value: Value) -> Value {
    if let Some(snapshot) = value.get_mut("snapshot").and_then(Value::as_object_mut) {
        snapshot.remove("files");
    }
    value
}

fn reconciled_commit(saved: &SavedOperation) -> Value {
    let mut result = saved.view.result.clone().unwrap_or_else(|| json!({}));
    result["reconciled"] = json!(true);
    if saved.push_after_commit {
        result["push"] = json!({"status":"unknown"});
    }
    result
}
