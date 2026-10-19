use super::*;

pub(super) enum Pair {
    Cargo(plan::Pair),
    Playwright(playwright::Pair),
}
impl Pair {
    pub(super) fn prepare(service: &ProcessService, value: &Value) -> Result<Self, String> {
        if value.get("playwright").is_some() {
            playwright::Pair::prepare(service, value).map(Self::Playwright)
        } else {
            plan::Pair::prepare(service, value).map(Self::Cargo)
        }
    }
    pub(super) fn workspace_enforced(&self) -> bool {
        match self {
            Self::Cargo(p) => p.workspace_enforced(),
            Self::Playwright(p) => p.workspace_enforced(),
        }
    }
    pub(super) fn status(&self) -> String {
        match self {
            Self::Cargo(p) => p.status(),
            Self::Playwright(p) => p.status(),
        }
    }
    pub(super) fn execute(
        self,
        service: &ProcessService,
        cancel: &CancelToken,
    ) -> Result<Value, String> {
        match self {
            Self::Cargo(p) => p.execute(service, cancel),
            Self::Playwright(p) => p.execute(service, cancel),
        }
    }
}
