//! The Agent Client Protocol backend that drives external AI coding agents.

mod connection;
pub mod map;
mod permissions;
mod runtime;

pub use map::{
    map_config_options, map_permission_options, map_prompt, map_stop_reason, map_tool_content,
    map_update,
};
pub use runtime::{AcpBackend, AcpConfig, AcpError, ThinkingDisplay};
