//! Reconcile control claims against committed original-session replay only.
use super::schedules::schedule_error;
use super::*;
use crate::{
    scheduler::{Delivery, DeliveryPhase, RulePhase},
    session::replay::{ReplayAdapter, ReplayEvent, ReplayTurnEnd},
};
impl TrustedProjectApplication {
    pub(crate) fn recover_automatic(&self) -> Result<(), ApplicationError> {
        if self.scheduler.is_none() {
            return Ok(());
        }
        let paused_jobs: std::collections::BTreeSet<_> = if self.jobs.is_none() {
            self.schedules()?
                .into_iter()
                .filter(|rule| matches!(rule.spec.trigger, crate::scheduler::Trigger::Job { .. }))
                .map(|r| r.id)
                .collect()
        } else {
            Default::default()
        };
        for delivery in self
            .schedule_deliveries()?
            .into_iter()
            .filter(|d| d.unsettled() && !paused_jobs.contains(&d.rule_id))
        {
            let receipt = self.automatic_receipt(&delivery.id)?;
            if let Some(receipt) = receipt {
                self.scheduler_service()?
                    .admitted(&self.schedule_project(), &delivery.id, receipt)
                    .map_err(schedule_error)?;
                let phase = self
                    .automatic_terminal(&delivery)?
                    .unwrap_or(DeliveryPhase::Interrupted);
                self.scheduler_service()?
                    .settle(
                        &self.schedule_project(),
                        &delivery.id,
                        phase,
                        None,
                        None,
                        crate::scheduler::now_ms(),
                    )
                    .map_err(schedule_error)?;
            } else if delivery.phase == DeliveryPhase::Admitted {
                self.scheduler_service()?
                    .settle(
                        &self.schedule_project(),
                        &delivery.id,
                        DeliveryPhase::Interrupted,
                        None,
                        None,
                        crate::scheduler::now_ms(),
                    )
                    .map_err(schedule_error)?;
            } else if !self
                .schedules()?
                .iter()
                .any(|r| r.id == delivery.rule_id && r.phase == RulePhase::Enabled)
            {
                self.scheduler_service()?
                    .settle(
                        &self.schedule_project(),
                        &delivery.id,
                        DeliveryPhase::Cancelled,
                        Some(Default::default()),
                        None,
                        crate::scheduler::now_ms(),
                    )
                    .map_err(schedule_error)?;
            }
        }
        Ok(())
    }
    fn automatic_terminal(
        &self,
        delivery: &Delivery,
    ) -> Result<Option<DeliveryPhase>, ApplicationError> {
        let key = self.session_key(&SessionId::new(&delivery.scope.session));
        let mut replay = ReplayAdapter::new();
        let mut rows = vec![];
        let mut admitted_turn = None;
        let mut phase = None;
        let client = format!("auto-{}", delivery.id);
        self.sessions
            .query_reader()
            .visit(&key, &mut |event| {
                replay.push(event, &mut rows);
                for row in rows.drain(..) {
                    match row {
                        ReplayEvent::UserMessage {
                            turn,
                            client_message_id: Some(id),
                            ..
                        } if id == client => {
                            admitted_turn = Some(turn);
                        }
                        ReplayEvent::TurnEnded { turn, reason, .. }
                            if admitted_turn == Some(turn) =>
                        {
                            phase = Some(match reason {
                                ReplayTurnEnd::Completed => DeliveryPhase::Completed,
                                ReplayTurnEnd::Aborted { .. } => DeliveryPhase::Cancelled,
                                ReplayTurnEnd::Interrupted => DeliveryPhase::Interrupted,
                                _ => DeliveryPhase::Failed,
                            });
                        }
                        _ => {}
                    }
                }
                Ok(())
            })
            .map_err(session_error)?;
        Ok(phase)
    }
}
