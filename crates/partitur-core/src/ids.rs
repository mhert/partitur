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

/// Identifies a repository within a project by its directory name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
        if value.is_empty() || value.contains(['/', '\\']) {
            Err(InvalidRepositoryId)
        } else {
            Ok(Self(value))
        }
    }
}

impl fmt::Display for RepositoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
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
}
