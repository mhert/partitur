//! The superpowers workflow (brainstorm, design, plan, execute, finish) as the UI shows it.
//!
//! Its display functions turn a session's [`WorkflowStatus`](crate::WorkflowStatus), the
//! tasks of its latest plan and the events it reported into the neutral display data of
//! [`workflow::display`](crate::workflow::display), in the wording every view shares. The
//! documents it works on are keyed `design` and `plan`.

mod display;

pub use display::{documents, gate_view, outline, rail, step_cell, transcript_row};
