use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PluginId(&'static str);

impl PluginId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Unforgeable ownership capability handed to a plugin only by its mounting
/// context. Contribution registries accept this token rather than a caller-
/// supplied PluginId, so a plugin cannot attribute effects to another owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PluginOwner(PluginId);

impl PluginOwner {
    pub(in crate::plugin) const fn new(id: PluginId) -> Self {
        Self(id)
    }

    pub(crate) const fn id(self) -> PluginId {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn for_test(id: PluginId) -> Self {
        Self(id)
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ServiceId(&'static str);

impl ServiceId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}
