//! Execute user-authorized model changes only at settled Goal boundaries.
use super::*;
use crate::goal::{GoalHandoff, HandoffRecord, HandoffRoute, HandoffSettings, LowAllowance};
use crate::model::ModelErrorKind;
use crate::providers::{AllowanceItem, ProviderAllowance};
use std::fmt::Write;

type Route = (ModelConfig, ProviderCredentials);

impl TrustedProjectApplication {
    pub(in crate::application) fn goal_execution_route(&self) -> Result<Route, ApplicationError> {
        let selected = self.model_state()?;
        let Some(queue) = self.goal()?.and_then(|goal| goal.state.handoff) else {
            return Ok(selected);
        };
        resolve_route(
            self.config.as_ref(),
            &queue.routes[queue.position],
            &selected,
        )
        .ok_or_else(|| {
            ApplicationError::new(
                "Goal model changed; review the handoff configuration before continuing",
            )
        })
    }

    pub(in crate::application) fn prepare_goal_handoff(
        &self,
        settings: &HandoffSettings,
        existing: Option<&GoalHandoff>,
    ) -> Result<GoalHandoff, String> {
        let (config, _) = if existing.is_some() {
            self.goal_execution_route()
                .map_err(|error| error.to_string())?
        } else {
            self.model_state().map_err(|error| error.to_string())?
        };
        if !(1..=99).contains(&settings.threshold)
            || settings.profiles.is_empty()
            || settings.note.len() > 16 * 1024
        {
            return Err("invalid Goal handoff profiles, threshold or note".into());
        }
        if !config.is_configured() {
            return Err("Goal handoff requires a configured current model".into());
        }
        let first = existing.map_or_else(
            || self.current_handoff_route(&config),
            |queue| queue.routes[queue.position].clone(),
        );
        let mut routes = vec![first];
        for profile in &settings.profiles {
            let (config, credentials) = self
                .config
                .load_profile(profile)
                .map_err(|e| e.to_string())?
                .ok_or("handoff profile no longer exists")?;
            let route = HandoffRoute::new(Some(profile.clone()), &config);
            if !config.is_configured()
                || routes
                    .iter()
                    .any(|prior| prior.profile.as_ref() == Some(profile))
            {
                return Err("handoff requires distinct configured model profiles".into());
            }
            self.providers
                .build(&config, &credentials)
                .map_err(|_| "handoff model is not configured")?;
            routes.push(route);
        }
        Ok(GoalHandoff {
            routes,
            threshold: settings.threshold,
            note: settings.note.trim().into(),
            position: 0,
        })
    }

    fn current_handoff_route(&self, config: &ModelConfig) -> HandoffRoute {
        let profile = self.config.active_profile().ok().flatten().filter(|name| {
            self.config
                .load_profile(name)
                .ok()
                .flatten()
                .is_some_and(|(saved, _)| &saved == config)
        });
        HandoffRoute::new(profile, config)
    }

    pub(in crate::application) fn workflow_configure_goal_handoff(
        &self,
        id: &str,
        revision: u64,
        settings: Option<&HandoffSettings>,
    ) -> Result<Value, String> {
        self.reject_session_switch_while_busy()
            .map_err(|e| e.to_string())?;
        let goal = self
            .goal()
            .map_err(|e| e.to_string())?
            .ok_or("no current Goal")?;
        if goal.armed || goal.state.id != id || goal.state.revision != revision {
            return Err("pause the Goal and refresh before editing handoff".into());
        }
        let handoff = settings
            .map(|settings| self.prepare_goal_handoff(settings, goal.state.handoff.as_ref()))
            .transpose()?;
        self.goal_service()
            .map_err(|e| e.to_string())?
            .configure_handoff(revision, handoff)?;
        Ok(json!({"ok": true}))
    }
}

fn resolve_route(
    store: &dyn crate::plugins::services::ConfigStore,
    route: &HandoffRoute,
    selected: &Route,
) -> Option<Route> {
    let found = match &route.profile {
        Some(name) => store.load_profile(name).ok().flatten()?,
        None => selected.clone(),
    };
    route.matches(&found.0).then_some(found)
}

pub(super) struct HandoffChoice {
    pub(super) position: usize,
    pub(super) record: HandoffRecord,
    pub(super) route: Option<Route>,
}

impl RunWorker {
    pub(super) fn goal_request_notice(&self) -> Option<crate::run::RequestNotice> {
        if !self.goal_mode {
            return None;
        }
        let goal = self.deps.goal_service.0.clone()?;
        let queue = goal.current().ok().flatten()?.state.handoff?;
        let monitor = self.deps.monitor.clone()?;
        let config = self.deps.config.clone();
        let credentials = self.deps.credentials.clone();
        Some(Box::new(move || {
            let allowance = monitor.goal_allowance(&config, &credentials, false);
            let (reason, window) = low_notice(&allowance, queue.threshold)?;
            goal.notify_low_allowance(window).then(|| {
                format!("Your Goal model allowance is low. {reason}\nFinish the current safe step, then promptly call update_goal with operation=handoff, the current expected_revision, and a short progress summary (or an empty string). The user has authorized the next model to continue this same Goal.")
            })
        }))
    }

    pub(super) fn choose_goal_handoff(
        &self,
        round: &round::SettledRound,
        accounting: &worker::RoundAccounting,
    ) -> Option<HandoffChoice> {
        if accounting.cancelled || self.deps.cancel.is_cancelled() {
            return None;
        }
        let goal = self.deps.goal_service.0.as_ref()?;
        let view = goal.current().ok().flatten()?;
        if !view.armed || view.state.phase != crate::goal::GoalPhase::Active {
            return None;
        }
        let queue = view.state.handoff?;
        let trigger = if accounting.succeeded {
            goal.take_handoff_request()
                .map(|(allowance, summary)| (Some(allowance), summary))
        } else if self.goal_quota_exhausted(round.model_kind) {
            Some((None, String::new()))
        } else {
            None
        }?;
        let selected = (self.deps.config.clone(), self.deps.credentials.clone());
        let mut choice = HandoffChoice {
            position: queue.position,
            route: None,
            record: HandoffRecord {
                from: selected.0.model.clone(),
                to: None,
                round: view.state.rounds_started,
                turn: self.input.turn,
                allowance: trigger.0,
                summary: trigger.1,
                skipped: Vec::new(),
            },
        };
        for (index, candidate) in queue.routes.iter().enumerate().skip(queue.position + 1) {
            if self.deps.cancel.is_cancelled() {
                return None;
            }
            match self.eligible_goal_route(candidate, &selected, queue.threshold) {
                Ok(route) => {
                    choice.position = index;
                    choice.record.to = Some(route.0.model.clone());
                    choice.route = Some(route);
                    break;
                }
                Err(reason) => choice
                    .record
                    .skipped
                    .push((candidate.model.clone(), reason.into())),
            }
        }
        Some(choice)
    }

    fn goal_quota_exhausted(&self, kind: Option<ModelErrorKind>) -> bool {
        match kind {
            Some(ModelErrorKind::QuotaExhausted) => true,
            Some(ModelErrorKind::RateLimited) => {
                self.deps.monitor.as_ref().is_some_and(|monitor| {
                    let value =
                        monitor.goal_allowance(&self.deps.config, &self.deps.credentials, true);
                    percentages(&value).any(|(_, remaining)| remaining == 0.0)
                })
            }
            _ => false,
        }
    }

    fn eligible_goal_route(
        &self,
        candidate: &HandoffRoute,
        selected: &Route,
        threshold: u8,
    ) -> Result<Route, &'static str> {
        let route = resolve_route(self.deps.config_store.as_ref(), candidate, selected)
            .ok_or("profile-changed")?;
        self.deps
            .providers
            .build(&route.0, &route.1)
            .map_err(|_| "profile-unavailable")?;
        if same_goal_account(&route, selected) {
            return Err("same-account");
        }
        let monitor = self.deps.monitor.as_ref().ok_or("quota-unknown")?;
        let sample = monitor.goal_allowance(&route.0, &route.1, false);
        let mut items = percentages(&sample).peekable();
        if items.peek().is_none() {
            return Err("quota-unknown");
        }
        if items.any(|(_, remaining)| remaining <= f64::from(threshold)) {
            return Err("quota-low");
        }
        Ok(route)
    }

    pub(super) fn apply_goal_route(&mut self, route: Route) {
        self.deps.config = route.0;
        self.deps.credentials = route.1;
        if let Some(service) = &self.deps.subagent_service {
            service.update_model(self.deps.config.clone(), self.deps.credentials.clone());
        }
        self.deps
            .slots
            .plugin_host
            .update_run_model(self.deps.config.clone(), self.deps.credentials.clone());
        self.context.runtime_context.model =
            Some(super::super::run_context::serving_model(&self.deps.config));
        let header = super::super::run_context::route_header(&self.deps.config);
        if let Some(current) = self.durable_request_header.header.as_object_mut() {
            current.extend(header);
        }
        self.restrict_goal_image_tool();
        if let Some(goal) = &self.deps.goal_service.0 {
            goal.reset_handoff_notice();
        }
    }

    fn restrict_goal_image_tool(&mut self) {
        if self.deps.config.capabilities.accepts_image_tool_results()
            || !self
                .context
                .tool_definitions
                .iter()
                .any(|tool| tool.name == "view_image")
        {
            return;
        }
        let access = self.context.tool_access.clone().with_view_image(false);
        let view = Arc::new(self.context.capabilities.restricted_to(&access));
        self.context.tool_definitions = view.definitions().into();
        self.context.capabilities = view.clone();
        self.context.tool_access = access.clone();
        self.deps.slots.tool_access.install(access);
        self.deps.slots.tools.install_capabilities(view);
        if let Some(tools) = self.durable_request_header.header["tools"].as_array_mut() {
            tools.retain(|tool| tool["name"] != "view_image");
        }
    }
}

fn same_goal_account(candidate: &Route, current: &Route) -> bool {
    let authority = |endpoint: &str| {
        endpoint
            .split_once("://")
            .and_then(|(_, address)| address.split('/').next())
            .map(str::to_ascii_lowercase)
    };
    candidate.1.value(0).is_some_and(|key| {
        !key.trim().is_empty()
            && current
                .1
                .value(0)
                .is_some_and(|previous| previous.trim() == key.trim())
    }) && authority(&candidate.0.endpoint) == authority(&current.0.endpoint)
}

fn percentages(allowance: &ProviderAllowance) -> impl Iterator<Item = (&AllowanceItem, f64)> {
    allowance.items.iter().filter_map(|item| {
        if allowance.status != "available"
            || item.kind != "quota"
            || item.unit.as_deref() != Some("%")
        {
            return None;
        }
        let remaining: f64 = item.value.parse().ok()?;
        (0.0..=100.0)
            .contains(&remaining)
            .then_some((item, remaining))
    })
}

fn low_notice(allowance: &ProviderAllowance, threshold: u8) -> Option<(String, LowAllowance)> {
    let (tightest, percent) = percentages(allowance)
        .filter(|(_, percent)| *percent <= f64::from(threshold))
        .min_by(|(_, a), (_, b)| a.total_cmp(b))?;
    let window = LowAllowance {
        window_hours: tightest.window_hours,
        remaining: percent.to_string(),
    };
    let mut lines = String::new();
    for (item, percent) in
        percentages(allowance).filter(|(_, value)| *value <= f64::from(threshold))
    {
        let reset = item
            .reset_at
            .and_then(|millis| i64::try_from(millis).ok())
            .and_then(|millis| jiff::Timestamp::from_millisecond(millis).ok())
            .map_or_else(|| "unknown".into(), |time| time.to_string());
        let window = item
            .window_hours
            .map_or_else(|| "unspecified window".into(), |hours| format!("{hours} h"));
        let _ = writeln!(lines, "{window}: {percent}% remaining; resets at {reset}.");
    }
    let sampled = allowance
        .updated_at
        .and_then(|ms| i64::try_from(ms).ok())
        .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
        .map_or_else(|| "unknown".into(), |time| time.to_string());
    Some((format!("Sampled at {sampled}.\n{lines}"), window))
}
