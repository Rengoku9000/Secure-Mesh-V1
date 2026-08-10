//! Records describing this node and, from Phase 2, the peers it has met.

use crate::error::{CoreError, CoreResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Reachability of a node from this node's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum NodeStatus {
    /// This node itself.
    Local,
    /// A peer currently reachable over the mesh.
    Online,
    /// A known peer that is not currently reachable.
    Offline,
}

impl NodeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeStatus::Local => "LOCAL",
            NodeStatus::Online => "ONLINE",
            NodeStatus::Offline => "OFFLINE",
        }
    }
}

impl fmt::Display for NodeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for NodeStatus {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "LOCAL" => Ok(NodeStatus::Local),
            "ONLINE" => Ok(NodeStatus::Online),
            "OFFLINE" => Ok(NodeStatus::Offline),
            _ => Err(CoreError::storage(format!(
                "database holds an unrecognised node status: {value}"
            ))),
        }
    }
}

/// A row in the `nodes` table.
///
/// `public_key` is the node's Ed25519 verifying key, hex-encoded. It is public
/// by design; no private material is ever stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRecord {
    pub id: String,
    pub node_name: String,
    pub public_key: String,
    pub status: NodeStatus,
    pub last_seen: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_status_round_trips_through_its_string_form() {
        for status in [NodeStatus::Local, NodeStatus::Online, NodeStatus::Offline] {
            assert_eq!(status.as_str().parse::<NodeStatus>().unwrap(), status);
        }
    }

    #[test]
    fn unknown_node_status_is_a_storage_error() {
        let err = "BANANA".parse::<NodeStatus>().unwrap_err();
        assert_eq!(err.code(), "STORAGE_ERROR");
    }

    #[test]
    fn node_status_serializes_as_an_uppercase_label() {
        assert_eq!(
            serde_json::to_string(&NodeStatus::Online).unwrap(),
            r#""ONLINE""#
        );
    }
}
