// SPDX-License-Identifier: AGPL-3.0-or-later

//! The proxy's confinement (§3.2 of docs/07-networking.md): it parses requests from anyone on
//! the internet, so before it reads one it gives up what it does not need. It sets
//! `no_new_privs`, drops every capability it holds, and confines itself with Landlock to no
//! filesystem access at all and no binding of a port; its sockets were handed to it already.
//! Each is per thread, so it is done while the proxy is still one thread, and every thread it
//! starts afterwards inherits them. What the kernel cannot apply is reported, never assumed.
//!
//! What it does not confine: which addresses it connects to. Landlock names ports, not
//! addresses, and a workload's port is any port; the node's own rules on what may leave the
//! proxy are the agent's to set.

use landlock::{ABI, Access as _, AccessFs, AccessNet, Ruleset, RulesetAttr as _, RulesetStatus};

/// What the confinement holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    /// Every capability was dropped, or none was held.
    pub capabilities: bool,
    /// Landlock confines the filesystem and binding.
    pub landlock: bool,
}

/// Confines the calling thread, which must be the process's only one.
pub fn confine() -> Result<Held, korpis_confine::ConfineError> {
    korpis_confine::no_new_privileges()?;
    let held = rustix::thread::capabilities(None)
        .map_err(|e| korpis_confine::ConfineError(format!("capabilities: {e}")))?;
    if !held.effective.is_empty() || !held.permitted.is_empty() {
        korpis_confine::keep_capabilities(&[])?;
    }
    let abi = ABI::V5;
    let landlock = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .and_then(|r| r.handle_access(AccessNet::BindTcp))
        .and_then(Ruleset::create)
        .and_then(landlock::RulesetCreated::restrict_self)
        .map_err(|e| korpis_confine::ConfineError(format!("landlock: {e}")))?;
    Ok(Held { capabilities: true, landlock: landlock.ruleset != RulesetStatus::NotEnforced })
}
