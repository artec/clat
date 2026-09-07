//! `SessionHeader` and its wire shape. Byte-exact port of
//! `session-persistence-jsonl/src/format.ts` HeaderLine across released v0
//! and v2: camelCase fields, optional fields wholly omitted (never null),
//! `delegationDepth` always written, retired policy fields rejected, and
//! format-version refusal BEFORE shape validation.

use crate::session::compat::SESSION_FORMAT_VERSION;
use crate::session::id::SessionId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionHeader {
    pub(crate) version: u32,
    pub(crate) id: SessionId,
    /// Unix epoch milliseconds, non-negative.
    pub(crate) created_at: i64,
    pub(crate) cwd: Option<String>,
    pub(crate) parent_session: Option<SessionId>,
    /// Released-v2 fork-lineage bit. Always present in a v2 physical header.
    pub(crate) is_seeded: bool,
    /// Released-v0 inherited-prefix cut. Preserved for legacy read-only logs;
    /// never written by the v2 writer.
    pub(crate) seed_length: Option<u64>,
    pub(crate) origin: Option<SessionOrigin>,
    /// Absent means zero; always written on the wire.
    pub(crate) delegation_depth: u32,
    pub(crate) agent_preset: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionOrigin {
    Subagent,
}

/// Wire order matches DSH `toHeaderLine` property insertion order exactly.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct V2HeaderLine {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    id: SessionId,
    #[serde(rename = "createdAt")]
    created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(rename = "parentSession", skip_serializing_if = "Option::is_none")]
    parent_session: Option<SessionId>,
    #[serde(rename = "isSeeded")]
    is_seeded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<SessionOrigin>,
    #[serde(rename = "delegationDepth")]
    delegation_depth: u32,
    #[serde(rename = "agentPreset", skip_serializing_if = "Option::is_none")]
    agent_preset: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct V0HeaderLine {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    id: SessionId,
    #[serde(rename = "createdAt")]
    created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(rename = "parentSession", skip_serializing_if = "Option::is_none")]
    parent_session: Option<SessionId>,
    #[serde(rename = "seedLength", skip_serializing_if = "Option::is_none")]
    seed_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<SessionOrigin>,
    #[serde(rename = "delegationDepth")]
    delegation_depth: u32,
    #[serde(rename = "agentPreset", skip_serializing_if = "Option::is_none")]
    agent_preset: Option<String>,
}

impl SessionOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Subagent => "subagent",
        }
    }
}

impl Serialize for SessionOrigin {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for SessionOrigin {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "subagent" => Ok(Self::Subagent),
            other => Err(serde::de::Error::custom(format!(
                "unknown origin `{other}`"
            ))),
        }
    }
}

/// A parsed header is not valid enough: retired policy baseline fields
/// (`sandboxMode` / `approvalPolicy`) must be rejected explicitly.
const RETIRED_FIELDS: [&str; 2] = ["sandboxMode", "approvalPolicy"];

impl SessionHeader {
    pub(crate) fn new(id: SessionId, cwd: Option<String>, created_at: i64) -> Self {
        Self {
            version: SESSION_FORMAT_VERSION,
            id,
            created_at,
            cwd,
            parent_session: None,
            is_seeded: false,
            seed_length: None,
            origin: None,
            delegation_depth: 0,
            agent_preset: None,
        }
    }

    /// Serialize to the single header JSON line (no trailing newline).
    pub(crate) fn to_line(&self) -> String {
        match self.version {
            0 => serde_json::to_string(&V0HeaderLine {
                kind: "session".into(),
                version: 0,
                id: self.id.clone(),
                created_at: self.created_at,
                cwd: self.cwd.clone(),
                parent_session: self.parent_session.clone(),
                seed_length: self.seed_length,
                origin: self.origin,
                delegation_depth: self.delegation_depth,
                agent_preset: self.agent_preset.clone(),
            })
            .expect("header is plain JSON"),
            2 => serde_json::to_string(&V2HeaderLine {
                kind: "session".into(),
                version: 2,
                id: self.id.clone(),
                created_at: self.created_at,
                cwd: self.cwd.clone(),
                parent_session: self.parent_session.clone(),
                is_seeded: self.is_seeded,
                origin: self.origin,
                delegation_depth: self.delegation_depth,
                agent_preset: self.agent_preset.clone(),
            })
            .expect("header is plain JSON"),
            version => panic!("cannot serialize unsupported session format v{version}"),
        }
    }

    /// Parse a header line (without newline). Mirrors DSH behavior:
    /// foreign format version refuses BEFORE shape validation; retired
    /// fields refuse; malformed lines return `None` (callers skip them in
    /// list contexts and reject them in load contexts).
    pub(crate) fn from_line(line: &str) -> Result<Option<Self>, HeaderError> {
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        // Version refusal precedes shape checks (compat doc §1).
        let version = value.get("version").and_then(|v| v.as_u64());
        if let Some(version) = version.filter(|version| !matches!(*version, 0 | 2)) {
            return Err(HeaderError::UnsupportedVersion(version as u32));
        }
        for field in RETIRED_FIELDS {
            if value.get(field).is_some() {
                return Err(HeaderError::RetiredField(field));
            }
        }
        match version {
            Some(0) => {
                if !is_common_header_line(&value) || value.get("isSeeded").is_some() {
                    return Ok(None);
                }
                let parsed: V0HeaderLine = serde_json::from_value(value)
                    .map_err(|error| HeaderError::Malformed(error.to_string()))?;
                Ok(Some(Self {
                    version: 0,
                    id: parsed.id,
                    created_at: parsed.created_at,
                    cwd: parsed.cwd,
                    parent_session: parsed.parent_session,
                    is_seeded: parsed.seed_length.is_some(),
                    seed_length: parsed.seed_length,
                    origin: parsed.origin,
                    delegation_depth: parsed.delegation_depth,
                    agent_preset: parsed.agent_preset,
                }))
            }
            Some(2) => {
                if !is_common_header_line(&value)
                    || !value.get("isSeeded").is_some_and(|v| v.is_boolean())
                    || value.get("seedLength").is_some()
                {
                    return Ok(None);
                }
                let parsed: V2HeaderLine = serde_json::from_value(value)
                    .map_err(|error| HeaderError::Malformed(error.to_string()))?;
                Ok(Some(Self {
                    version: 2,
                    id: parsed.id,
                    created_at: parsed.created_at,
                    cwd: parsed.cwd,
                    parent_session: parsed.parent_session,
                    is_seeded: parsed.is_seeded,
                    seed_length: None,
                    origin: parsed.origin,
                    delegation_depth: parsed.delegation_depth,
                    agent_preset: parsed.agent_preset,
                }))
            }
            _ => Ok(None),
        }
    }
}

#[derive(Debug)]
pub(crate) enum HeaderError {
    UnsupportedVersion(u32),
    RetiredField(&'static str),
    Malformed(String),
}

/// Shape checks from `isHeaderLine`: numbers must be non-negative safe
/// integers (`-0` cannot survive JSON round-trip in serde_json, matching
/// DSH's `!Object.is(x, -0)` intent).
fn is_common_header_line(value: &serde_json::Value) -> bool {
    // JS Number.isSafeInteger: |x| <= 2^53 - 1 (millisecond epochs live here).
    const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
    let is_safe_non_negative = |value: &serde_json::Value| matches!(value.as_i64(), Some(number) if (0..=MAX_SAFE_INTEGER).contains(&number));
    value.get("type").and_then(|v| v.as_str()) == Some("session")
        && value.get("version").is_some_and(|v| v.is_u64())
        && value.get("id").is_some_and(|v| v.is_string())
        && value.get("createdAt").is_some_and(is_safe_non_negative)
        && value
            .get("delegationDepth")
            .is_some_and(is_safe_non_negative)
        && value
            .get("origin")
            .is_none_or(|v| v.as_str() == Some("subagent"))
        && value.get("agentPreset").is_none_or(|v| v.is_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SessionHeader {
        SessionHeader::new(
            SessionId::new("018f2a64-9d3f-7cde-8123-9a4f2b6c0001"),
            Some("/Users/deng/Documents/GitHub/clat".into()),
            1_723_980_000_000,
        )
    }

    #[test]
    fn wire_shape_matches_dsh_field_order_and_omission() {
        let line = sample().to_line();
        assert_eq!(
            line,
            "{\"type\":\"session\",\"version\":2,\"id\":\"018f2a64-9d3f-7cde-8123-9a4f2b6c0001\",\"createdAt\":1723980000000,\"cwd\":\"/Users/deng/Documents/GitHub/clat\",\"isSeeded\":false,\"delegationDepth\":0}"
        );
        let back = SessionHeader::from_line(&line)
            .expect("parse")
            .expect("header");
        assert_eq!(back, sample());
    }

    #[test]
    fn optional_fields_are_omitted_never_null() {
        let minimal = SessionHeader::new(SessionId::new("x"), None, 5);
        let line = minimal.to_line();
        assert!(!line.contains("cwd"));
        assert!(!line.contains("agentPreset"));
        assert!(line.contains("\"isSeeded\":false"));
        assert!(line.contains("\"delegationDepth\":0"));
    }

    #[test]
    fn retired_policy_fields_are_rejected() {
        let line = sample().to_line().replace(
            "\"delegationDepth\":0",
            "\"delegationDepth\":0,\"sandboxMode\":\"restricted\"",
        );
        assert!(matches!(
            SessionHeader::from_line(&line),
            Err(HeaderError::RetiredField("sandboxMode"))
        ));
    }

    #[test]
    fn foreign_version_refuses_before_shape_checks() {
        // Malformed apart from the version: the version refusal must still
        // win, exactly like DSH's refuseForeignFormatVersion.
        let line = "{\"type\":\"garbage\",\"version\":3}";
        assert!(matches!(
            SessionHeader::from_line(line),
            Err(HeaderError::UnsupportedVersion(3))
        ));
    }

    #[test]
    fn released_v0_remains_readable_but_v2_requires_is_seeded() {
        let v0 = "{\"type\":\"session\",\"version\":0,\"id\":\"legacy\",\"createdAt\":1,\"seedLength\":4,\"delegationDepth\":0}";
        let parsed = SessionHeader::from_line(v0).unwrap().unwrap();
        assert_eq!(parsed.version, 0);
        assert_eq!(parsed.seed_length, Some(4));
        assert!(parsed.is_seeded);
        let missing = "{\"type\":\"session\",\"version\":2,\"id\":\"new\",\"createdAt\":1,\"delegationDepth\":0}";
        assert!(SessionHeader::from_line(missing).unwrap().is_none());
    }

    #[test]
    fn non_header_lines_return_none_for_callers_to_skip_or_reject() {
        assert_eq!(SessionHeader::from_line("{}").expect("parse"), None);
        assert_eq!(
            SessionHeader::from_line("{\"type\":\"other\"}").expect("parse"),
            None
        );
        // Negative createdAt fails the shape check.
        let bad = sample().to_line().replace("1723980000000", "-5");
        assert_eq!(SessionHeader::from_line(&bad).expect("parse"), None);
    }
}
