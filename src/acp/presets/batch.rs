use super::super::{MAX_AGENTS, new_id};
use super::{AddPreset, Agent, AgentEdit, Catalog, ControlStorage, PRESETS, Preset, Service};
use super::{Search, UiMessage, existing, operation_error, state};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path, thread};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AddPresets {
    presets: Vec<AddPreset>,
}

type PresetResult = Result<Value, UiMessage>;

struct PreparedPreset<'a> {
    preset: &'static Preset,
    detection: &'a str,
    edit: AgentEdit,
    fingerprint: String,
}

impl Service {
    pub(crate) fn add_agent_presets(
        &self,
        control: &ControlStorage,
        project: &Path,
        request: AddPresets,
    ) -> Result<Value, UiMessage> {
        let search = Search::host(project);
        let results = self.add_presets_from(control, &search, &request.presets)?;
        Ok(Value::Array(
            request
                .presets
                .iter()
                .zip(results)
                .map(|(request, result)| match result {
                    Ok(agent) => json!({"id": request.id, "agent": agent}),
                    Err(error) => json!({"id": request.id, "error": error}),
                })
                .collect(),
        ))
    }

    pub(super) fn add_presets_from(
        &self,
        control: &ControlStorage,
        search: &Search<'_>,
        requests: &[AddPreset],
    ) -> Result<Vec<PresetResult>, UiMessage> {
        if requests.is_empty() || requests.len() > PRESETS.len() {
            return Err(UiMessage::new("acp.invalid_request"));
        }
        let unique = requests
            .iter()
            .map(|request| &request.id)
            .collect::<HashSet<_>>();
        if unique.len() != requests.len() {
            return Err(UiMessage::new("acp.invalid_request"));
        }
        let prepared = prepare_batch(search, requests);
        if prepared.iter().all(Result::is_err) {
            return Ok(prepared
                .into_iter()
                .filter_map(Result::err)
                .map(Err)
                .collect());
        }
        self.change(control, |catalog| {
            Ok(prepared
                .into_iter()
                .map(|item| item.and_then(|item| item.register(search, catalog)))
                .collect())
        })
        .map_err(operation_error)
    }
}

fn prepare_batch<'a>(
    search: &Search<'_>,
    requests: &'a [AddPreset],
) -> Vec<Result<PreparedPreset<'a>, UiMessage>> {
    if let [request] = requests {
        return vec![prepare(search, request)];
    }
    // Keep simultaneous executable reads and SHA-256 work bounded.
    requests
        .chunks(4)
        .flat_map(|group| {
            thread::scope(|scope| {
                let jobs: Vec<_> = group
                    .iter()
                    .map(|request| {
                        let job = thread::Builder::new()
                            .spawn_scoped(scope, move || prepare(search, request));
                        (request, job)
                    })
                    .collect();
                jobs.into_iter()
                    .map(|(request, job)| match job {
                        Ok(handle) => handle.join().unwrap_or_else(|_| {
                            Err(operation_error("ACP executable inspection failed".into()))
                        }),
                        Err(_) => prepare(search, request),
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

fn prepare<'a>(
    search: &Search<'_>,
    request: &'a AddPreset,
) -> Result<PreparedPreset<'a>, UiMessage> {
    let preset = PRESETS
        .iter()
        .find(|preset| preset.id == request.id)
        .ok_or_else(|| UiMessage::new("acp.invalid_request"))?;
    let candidate = search.exact(preset, &request.detection)?;
    let edit = preset.edit(&candidate).checked().map_err(operation_error)?;
    if edit.program != candidate.path {
        return Err(UiMessage::new("acp.preset_changed"));
    }
    let fingerprint = state::fingerprint(&edit.program).map_err(operation_error)?;
    Ok(PreparedPreset {
        preset,
        detection: &request.detection,
        edit,
        fingerprint,
    })
}

impl PreparedPreset<'_> {
    fn register(self, search: &Search<'_>, catalog: &mut Catalog) -> PresetResult {
        // Revalidate after hashing and just before accepting the configuration.
        search.exact(self.preset, self.detection)?;
        let project = search.project.to_str().unwrap_or_default();
        if let Some(agent) = existing(catalog, &self.edit) {
            return Ok(agent.view(project));
        }
        if catalog.agents.len() >= MAX_AGENTS {
            return Err(operation_error("ACP Agent limit reached".into()));
        }
        let agent = Agent::configured(new_id(), self.edit, self.fingerprint);
        let view = agent.view(project);
        catalog.agents.insert(agent.id.clone(), agent);
        Ok(view)
    }
}
