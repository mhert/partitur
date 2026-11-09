//! The Agent Client Protocol backend that drives external AI coding agents.

mod connection;
pub mod map;
mod mcp;
mod permissions;
mod runtime;
mod tools;

pub use map::{
    SessionState, map_config_options, map_permission_options, map_prompt, map_stop_reason,
    map_update,
};
pub use runtime::{
    ADAPTER_COMMAND, ADAPTER_PACKAGE, AcpBackend, AcpConfig, AcpError, ThinkingDisplay,
};
