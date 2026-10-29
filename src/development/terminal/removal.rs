//! Registry publication and compact tombstones are one atomic private document.
use super::*;
impl Terminals {
    pub(super) fn remove(&self, method: &str, value: &Value) -> Result<Value, String> {
        let operation = identity(value, "operationId")?;
        let signature = digest(format!("{method}:{value}").as_bytes());
        let mut registry: Registry = self.store.load("terminals.json")?;
        if let Some((old, result)) = registry.receipts.get(&operation) {
            return if *old == signature {
                Ok(result.clone())
            } else {
                Err("operation_conflict".into())
            };
        }
        let bulk = method == "terminal.removeEnded";
        let id = if bulk {
            None
        } else {
            Some(identity(value, "id")?)
        };
        let mut entries = self.entries.lock().map_err(|_| "terminal_busy")?;
        let mut removed = Vec::new();
        for row in &registry.terminals {
            if id.as_ref().is_some_and(|id| *id != row.id) {
                continue;
            }
            if let Some(entry) = entries.get(&row.id) {
                if bulk && !entry.closed.load(Ordering::SeqCst) {
                    continue;
                }
                if !entry.closed.load(Ordering::SeqCst) && value["confirmed"] != true {
                    return Err("confirmation_required".into());
                }
                entry.stop_checked()?;
                entry.close();
            }
            removed.push(row.id.clone());
        }
        for row in &registry.terminals {
            if removed.contains(&row.id) {
                registry
                    .removed
                    .insert(row.id.clone(), row.signature.clone());
            }
        }
        registry.terminals.retain(|row| !removed.contains(&row.id));
        let result = json!({"removed":removed});
        registry
            .receipts
            .insert(operation, (signature, result.clone()));
        self.store.save("terminals.json", &registry)?;
        for id in removed {
            entries.remove(&id);
        }
        Ok(result)
    }
    pub(super) fn rename(&self, value: &Value) -> Result<Value, String> {
        let id = identity(value, "id")?;
        let name = text(value, "name")?;
        if name.trim().is_empty() || name.len() > 200 {
            return Err("invalid_name".into());
        }
        let mut registry: Registry = self.store.load("terminals.json")?;
        let row = registry
            .terminals
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or("terminal_missing")?;
        row.name = name.into();
        self.store.save("terminals.json", &registry)?;
        if let Some(entry) = self.entries.lock().map_err(|_| "terminal_busy")?.get(&id) {
            entry.metadata.lock().map_err(|_| "terminal_busy")?.name = name.into();
        }
        Ok(json!({}))
    }
}
impl Entry {
    fn stop_checked(&self) -> Result<(), String> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut platform = self.platform.lock().map_err(|_| "terminal_busy")?;
        platform.terminate()?;
        if !platform.ended()? {
            return Err("terminal_busy".into());
        }
        *platform = Box::new(platform::Ended);
        self.closed.store(true, Ordering::SeqCst);
        self.metadata.lock().map_err(|_| "terminal_busy")?.phase = "ended".into();
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    struct FailOnce {
        inner: Box<dyn platform::Control>,
        fail: bool,
    }
    impl platform::Control for FailOnce {
        fn resize(&mut self, r: u16, c: u16) -> Result<(), String> {
            self.inner.resize(r, c)
        }
        fn ended(&mut self) -> Result<bool, String> {
            self.inner.ended()
        }
        fn terminate(&mut self) -> Result<(), String> {
            if self.fail {
                self.fail = false;
                Err("injected_termination_failure".into())
            } else {
                self.inner.terminate()
            }
        }
    }
    #[test]
    fn failed_process_shutdown_retains_the_terminal_and_allows_retry() {
        let base =
            std::env::temp_dir().join(format!("clat-terminal-failure-{}", uuid::Uuid::new_v4()));
        let root = base.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let service = Arc::new(DevelopmentService::new(
            Project::new(&root),
            &base.join("private"),
        ));
        service
            .request(
                "terminal.create",
                json!({"operationId":"failure","clientId":"owner","shell":"/bin/sh"}),
                None,
            )
            .unwrap();
        let entry = service.terminals.entry("failure").unwrap();
        {
            let mut platform = entry.platform.lock().unwrap();
            let inner = std::mem::replace(&mut *platform, Box::new(platform::Ended));
            *platform = Box::new(FailOnce { inner, fail: true });
        }
        let request = json!({"operationId":"remove-failure","id":"failure","confirmed":true});
        assert_eq!(
            service
                .request("terminal.remove", request.clone(), None)
                .unwrap_err(),
            "injected_termination_failure"
        );
        assert_eq!(
            service.request("terminal.list", json!({}), None).unwrap()["terminals"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(service.has_live_work());
        service.request("terminal.remove", request, None).unwrap();
        assert!(!service.has_live_work());
        assert!(
            service.request("terminal.list", json!({}), None).unwrap()["terminals"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        service.close();
        drop(entry);
        drop(service);
        crate::test_support::cleanup_tree(&base);
    }
}
