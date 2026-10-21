//! One Task terminal after cleanup, accounting, acceptance and durable close.
use super::*;
pub(super) fn finish(
    result: ApplicationRunResult,
    task: &crate::task::TaskService,
    run: &str,
    turn: u64,
    cancel: &CancelToken,
    recorder: &Mutex<SessionRecorder>,
    sink: &Mutex<Box<dyn EventSink + Send>>,
) -> ApplicationRunResult {
    let mut result = result;
    if cancel.is_cancelled()
        && let Ok(done) = &mut result
    {
        done.cancelled = true;
    }
    let successful = result.as_ref().is_ok_and(|d| !d.cancelled);
    let end_reason = reason(&result);
    let close = NewSessionEvent::new("turn/end", payloads::turn_end(turn, &end_reason));
    let committed = task.finalize_candidate_at_terminal(run, successful, cancel, &[close]);
    let (result, events) = {
        let mut recorder = recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match committed {
            Ok(()) => {
                recorder.emit_and_forward(final_event(&result));
                let (error, events) = recorder.terminal_committed();
                (merge(result, error), events)
            }
            Err(error) => {
                let result = merge(
                    result,
                    Some(format!("task completion gate failed: {error}")),
                );
                let message = result.as_ref().unwrap_err().error.clone();
                recorder.force_terminal_failure(message);
                let (journal_error, events) = recorder.finish(reason(&result));
                (merge(result, journal_error), events)
            }
        }
    };
    for event in events {
        let mut sink = sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if catch_unwind(AssertUnwindSafe(|| sink.emit(event))).is_err() {
            eprintln!("clat: warning: frontend event sink panicked during terminal publication");
        }
    }
    result
}
fn reason(result: &ApplicationRunResult) -> TurnEndReason {
    match result {
        Ok(done) if done.cancelled => TurnEndReason::Aborted {
            reason: TurnEndCancelCause::User,
        },
        Ok(_) => TurnEndReason::Completed,
        Err(f) => TurnEndReason::Error {
            error: json!({"message":f.error}),
        },
    }
}
fn merge(result: ApplicationRunResult, error: Option<String>) -> ApplicationRunResult {
    match (result, error) {
        (result, None) => result,
        (Ok(done), Some(error)) => Err(ApplicationRunFailure {
            receipt: done.receipt,
            error,
            turns: done.turns,
            usage: done.usage,
        }),
        (Err(mut f), Some(error)) => {
            f.error.push_str(&format!("; {error}"));
            Err(f)
        }
    }
}

fn final_event(result: &ApplicationRunResult) -> RunEvent {
    match result {
        Ok(d) if d.cancelled => RunEvent::RunCancelled {
            turns: d.turns,
            usage: d.usage.clone(),
        },
        Ok(d) => RunEvent::RunCompleted {
            output: d.output.clone(),
            turns: d.turns,
            usage: d.usage.clone(),
        },
        Err(f) => RunEvent::RunFailed {
            message: f.error.clone(),
        },
    }
}
