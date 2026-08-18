//! `SessionId` — an opaque string in DSH (unvalidated branded string).
//! CLAT generates UUIDs and preserves foreign ids verbatim; the id is safe
//! for filesystem use only after `path_layout::encode_segment`.

use std::fmt;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Fresh id, UUID v4 (mirrors DSH's crypto.randomUUID-based ids).
    pub fn generate() -> Self {
        Self::new(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl serde::Serialize for SessionId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(SessionId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_uuids_and_round_trip_through_json() {
        let id = SessionId::generate();
        assert_eq!(id.as_str().len(), 36);
        let value = serde_json::to_value(&id).expect("serialize");
        let back: SessionId = serde_json::from_value(value).expect("deserialize");
        assert_eq!(back, id);
    }

    #[test]
    fn foreign_ids_are_preserved_verbatim() {
        let id = SessionId::new("any opaque/../string");
        assert_eq!(id.as_str(), "any opaque/../string");
    }
}
