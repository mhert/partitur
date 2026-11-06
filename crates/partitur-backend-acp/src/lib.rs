//! The Agent Client Protocol backend that drives external AI coding agents.

pub mod map;

pub use map::{
    map_config_options, map_permission_options, map_prompt, map_stop_reason, map_tool_content,
    map_update,
};
