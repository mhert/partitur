//! The profile of any agent Partitur has no profile of its own for.
//!
//! [`Generic`] overrides none of [`AgentProfile`]'s hooks, so such an agent is driven as one
//! that speaks ACP as it is: none of its updates are dropped, no title names a tool, every
//! prompt goes out at once, an orchestrator is kept from editing and running commands by its
//! denied permission requests alone, and it is kept asking by its config options. A
//! connection also uses it until the agent's `initialize` answer names the agent.

use super::AgentProfile;

/// Any agent without a profile of its own.
pub(crate) struct Generic;

impl AgentProfile for Generic {
    fn label(&self) -> &'static str {
        "generic"
    }
}
