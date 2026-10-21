//! Optional producer-owned declarations. They confer no execution authority.
use std::sync::Weak;

#[derive(Clone, Default)]
pub struct ToolCapability {
    pub source: &'static str,
    pub adapter: String,
    pub tags: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub scope: &'static str,
    pub limits: String,
    /// Host handler availability only; never end-to-end readiness.
    pub host_available: bool,
    pub(crate) lifetime: Option<Weak<()>>,
}
impl ToolCapability {
    pub fn native(tags: &[&str], inputs: &[&str], outputs: &[&str], limits: &str) -> Self {
        Self {
            source: "native",
            adapter: "native".into(),
            tags: strings(tags),
            inputs: strings(inputs),
            outputs: strings(outputs),
            scope: "local",
            limits: limits.into(),
            host_available: true,
            lifetime: None,
        }
    }
    pub(crate) fn external(source: &'static str, adapter: &str, scope: &'static str) -> Self {
        Self { source, adapter: adapter.into(), scope,
            limits: "Provider declaration only; dependencies, login and end-to-end health unobserved. Check /mcp or plugin management; login/configuration/install require user action.".into(),
            ..Self::default() }
    }
    pub(crate) fn bounded(mut self) -> Self {
        self.adapter = super::query::clip(&self.adapter, 128);
        self.limits = super::query::clip(&self.limits, 256);
        for tags in [&mut self.tags, &mut self.inputs, &mut self.outputs] {
            tags.truncate(8);
            for tag in tags {
                *tag = super::query::clip(tag, 64);
            }
        }
        self
    }
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).into()).collect()
}
