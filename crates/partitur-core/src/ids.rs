//! Typed identifiers shared across Partitur, so a session id can never be passed where a
//! repository id is expected.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Defines a newtype around a [`Uuid`] that generates v4 ids and parses from text.
macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a fresh random (v4) id.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::from_str(s).map(Self)
            }
        }
    };
}

/// Defines a newtype around an opaque, backend- or protocol-supplied string.
macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps the given value.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the id as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

/// Defines a newtype around a `u64` counter id.
macro_rules! numeric_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

uuid_id!(
    /// Identifies a project, stable across restarts and rescans.
    ProjectId
);
uuid_id!(
    /// Identifies one agent session within a project.
    SessionId
);
uuid_id!(
    /// Identifies one proposal of child sessions within an orchestrator's transcript.
    ProposalId
);

string_id!(
    /// The session identifier a backend assigned, opaque to Partitur.
    BackendSessionId
);
string_id!(
    /// Identifies one tool call within a session, as reported by the backend.
    ToolCallId
);
string_id!(
    /// Identifies a configurable option (for example the model) offered by a backend.
    ConfigId
);
string_id!(
    /// Identifies one selectable value of a [`ConfigId`] option.
    ConfigValueId
);
string_id!(
    /// Identifies one choice offered in a permission request.
    PermissionOptionId
);

numeric_id!(
    /// Correlates a request sent to a backend with its response.
    RequestId
);
numeric_id!(
    /// Correlates a permission prompt with the user's decision.
    PermissionId
);
numeric_id!(
    /// Correlates an orchestrator tool call the backend parked with the core's answer.
    ToolRequestId
);

/// Correlates a request queued through the [`Git`](crate::Git) port with the event that
/// answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GitRequestId(pub u64);

impl fmt::Display for GitRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "git-{}", self.0)
    }
}

/// Identifies a repository within a project by its directory name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct RepositoryId(String);

/// The rejection reason for [`RepositoryId`]'s `TryFrom<&str>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("repository id must be non-empty and contain no path separators")]
pub struct InvalidRepositoryId;

impl RepositoryId {
    /// Returns the id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for RepositoryId {
    type Error = InvalidRepositoryId;

    /// # Errors
    ///
    /// Returns [`InvalidRepositoryId`] when `value` is empty or contains `/` or `\`.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

impl TryFrom<String> for RepositoryId {
    type Error = InvalidRepositoryId;

    /// Also the path taken when deserializing, so stored ids cannot violate the invariant.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRepositoryId`] when `value` is empty or contains `/` or `\`.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if is_valid_name(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidRepositoryId)
        }
    }
}

impl fmt::Display for RepositoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `value` is usable as a repository or backend id: non-empty and free of path
/// separators.
fn is_valid_name(value: &str) -> bool {
    !value.is_empty() && !value.contains(['/', '\\'])
}

/// Identifies a configured agent backend by the name the configuration gives it.
///
/// The name follows the same rule as a [`RepositoryId`]: non-empty, without `/` or `\`.
///
/// # Examples
///
/// ```
/// use partitur_core::{BackendId, InvalidBackendId};
///
/// let id = BackendId::try_from("claude-work")?;
/// assert_eq!(id.to_string(), "claude-work");
/// assert!(!id.is_unset());
/// assert_eq!(BackendId::try_from("a/b"), Err(InvalidBackendId));
/// # Ok::<(), InvalidBackendId>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct BackendId(String);

/// The rejection reason for [`BackendId`]'s `TryFrom<&str>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("backend id must be non-empty and contain no path separators")]
pub struct InvalidBackendId;

impl BackendId {
    /// The empty id a [`Session`](crate::Session) carries when it is deserialized without a
    /// `backend` field, as sessions saved before sessions named their backend are. It never
    /// passes validation, so only the core can create it (here and through that
    /// deserialization). Such a session is never saved until [`App::load`](crate::App::load)
    /// has replaced the id with the configured default.
    pub(crate) fn unset() -> Self {
        Self(String::new())
    }

    /// Wraps a fixed name known to follow the rule, for the core's test fixtures.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn trusted(name: &'static str) -> Self {
        Self(name.to_owned())
    }

    /// Whether this is the placeholder of a session saved before sessions named their
    /// backend.
    pub fn is_unset(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for BackendId {
    type Error = InvalidBackendId;

    /// # Errors
    ///
    /// Returns [`InvalidBackendId`] when `value` is empty or contains `/` or `\`.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

impl TryFrom<String> for BackendId {
    type Error = InvalidBackendId;

    /// Also the path taken when deserializing, so stored ids cannot violate the invariant.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidBackendId`] when `value` is empty or contains `/` or `\`.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if is_valid_name(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidBackendId)
        }
    }
}

impl fmt::Display for BackendId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A backend the configuration names, as the core and the UI know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    /// The backend's id, which sessions record.
    pub id: BackendId,
    /// The name shown to the user; the id for now.
    pub name: String,
    /// The command that starts the backend's adapter, shown in the environment view.
    pub command: String,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;

    #[test]
    fn repository_ids_reject_empty_and_separators() {
        assert_eq!(RepositoryId::try_from(""), Err(InvalidRepositoryId));
        assert_eq!(RepositoryId::try_from("a/b"), Err(InvalidRepositoryId));
        assert_eq!(RepositoryId::try_from("a\\b"), Err(InvalidRepositoryId));
        assert_eq!(RepositoryId::try_from("api").unwrap().as_str(), "api");
    }

    #[test]
    fn repository_ids_validate_when_deserialized() {
        let id: RepositoryId = serde_json::from_str("\"api\"").unwrap();
        assert_eq!(id.as_str(), "api");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"api\"");
        assert!(serde_json::from_str::<RepositoryId>("\"\"").is_err());
        assert!(serde_json::from_str::<RepositoryId>("\"a/b\"").is_err());
        assert!(serde_json::from_str::<RepositoryId>("\"a\\\\b\"").is_err());
    }

    #[test]
    fn invalid_repository_id_error_names_the_rule() {
        assert_eq!(
            InvalidRepositoryId.to_string(),
            "repository id must be non-empty and contain no path separators"
        );
    }

    #[test]
    fn backend_ids_follow_the_repository_rule_and_unset_is_empty() {
        assert_eq!(BackendId::try_from(""), Err(InvalidBackendId));
        assert_eq!(BackendId::try_from("a/b"), Err(InvalidBackendId));
        assert_eq!(BackendId::try_from("a\\b"), Err(InvalidBackendId));
        let id = BackendId::try_from("claude-work").unwrap();
        assert_eq!(id.as_str(), "claude-work");
        assert_eq!(id.to_string(), "claude-work");
        assert!(!id.is_unset());
        assert_eq!(
            BackendId::try_from(String::from("claude"))
                .unwrap()
                .as_str(),
            "claude"
        );
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"claude-work\"");
        assert_eq!(
            serde_json::from_str::<BackendId>("\"claude-work\"").unwrap(),
            id
        );
        assert!(serde_json::from_str::<BackendId>("\"\"").is_err());
        assert!(serde_json::from_str::<BackendId>("\"a/b\"").is_err());
        assert!(BackendId::unset().is_unset());
        assert_eq!(BackendId::unset().as_str(), "");
        assert_eq!(
            InvalidBackendId.to_string(),
            "backend id must be non-empty and contain no path separators"
        );
    }

    #[test]
    fn project_and_session_ids_round_trip_through_display_and_from_str() {
        let project = ProjectId::new();
        assert_eq!(project.to_string().parse::<ProjectId>().unwrap(), project);
        let session = SessionId::new();
        assert_eq!(session.to_string().parse::<SessionId>().unwrap(), session);
    }

    #[test]
    fn new_ids_are_distinct_and_default_generates_a_fresh_one() {
        assert_ne!(ProjectId::new(), ProjectId::new());
        assert_ne!(SessionId::new(), SessionId::new());
        assert_ne!(ProjectId::default(), ProjectId::default());
        assert_ne!(SessionId::default(), SessionId::default());
    }

    #[test]
    fn malformed_uuids_do_not_parse() {
        assert!("not-a-uuid".parse::<ProjectId>().is_err());
        assert!("not-a-uuid".parse::<SessionId>().is_err());
    }

    #[test]
    fn string_ids_display_their_value() {
        assert_eq!(BackendSessionId::new("s-1").to_string(), "s-1");
        assert_eq!(ToolCallId::new("t-1").to_string(), "t-1");
        assert_eq!(ConfigId::new("model").to_string(), "model");
        assert_eq!(ConfigValueId::new("fast").to_string(), "fast");
        assert_eq!(PermissionOptionId::new("allow").to_string(), "allow");
        assert_eq!(BackendSessionId::new("s-1").as_str(), "s-1");
        assert_eq!(RepositoryId::try_from("api").unwrap().to_string(), "api");
    }

    #[test]
    fn numeric_ids_display_their_value_and_are_copy() {
        let request = RequestId(7);
        let copy = request;
        assert_eq!(request.to_string(), "7");
        assert_eq!(copy, request);
        let permission = PermissionId(9);
        let copy = permission;
        assert_eq!(permission.to_string(), "9");
        assert_eq!(copy, permission);
    }

    #[test]
    fn git_request_ids_display_with_a_prefix_and_order_by_number() {
        let request = GitRequestId(12);
        let copy = request;
        assert_eq!(request.to_string(), "git-12");
        assert_eq!(copy, request);
        assert!(GitRequestId(2) < GitRequestId(10));
    }
}
