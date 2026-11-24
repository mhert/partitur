//! The Agent Client Protocol backend that drives external AI coding agents.

mod connection;
mod connection_v1;
pub mod map;
mod permissions;
mod runtime;
mod v1;

#[doc(hidden)]
pub use connection::INSTRUCTIONS_DIRS_VAR;
pub use map::{
    SessionState, map_config_options, map_permission_options, map_prompt, map_stop_reason,
    map_update,
};
pub use runtime::{
    ADAPTER_COMMAND, ADAPTER_PACKAGE, AcpBackend, AcpConfig, AcpError, INSTRUCTIONS_FILE,
    Resolution, ThinkingDisplay, write_instructions,
};
