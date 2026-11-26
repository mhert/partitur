//! The widget engine's shared ground: what the Partitur process and the helper process that
//! renders MCP App widgets agree on. The helper is driven over its standard input and output,
//! and the [`protocol`] module is the language they speak.

pub mod protocol;
