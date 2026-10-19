// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whether a chain is in force (§3.5 of docs/08-identity.md): what decides that a workload holds
//! authority, and with it whether its node relays the API to it (§9.5 there).

use crate::support::{Tree, child, days, root};
use korpis_core::authz::Scope;
use korpis_core::authz::record::{GrantRecord, in_force};
use korpis_core::authz::{Conditions, Grant};
use korpis_core::principal::Principal;
use std::time::SystemTime;

fn kept(grant: Grant, now: SystemTime) -> GrantRecord {
    GrantRecord {
        grant,
        holder: Principal::Anonymous,
        issued_by: Principal::Anonymous,
        issued_at: now,
        revoked_at: None,
        suspended_at: None,
        label: String::new(),
        from_template: None,
    }
}

#[test]
fn a_chain_is_in_force_only_while_every_grant_in_it_is() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let top = root(&tree, now);
    let held = child(&top, "workload:w", &["backup.create"], Scope::Object(tree.host));
    let chain = [kept(top.clone(), now), kept(held.clone(), now)];
    assert!(in_force(&chain, now));
    assert!(!in_force(&[], now), "no chain holds nothing");

    let mut revoked = chain.clone();
    revoked[0].revoked_at = Some(now);
    assert!(!in_force(&revoked, now), "an ancestor's revocation reaches the leaf");
    let mut suspended = chain.clone();
    suspended[1].suspended_at = Some(now);
    assert!(!in_force(&suspended, now));

    let expiring = Conditions { expires_at: Some(now + days(1)), ..Conditions::default() };
    let leaf = kept(Grant { conditions: expiring, ..held }, now);
    let chain = [kept(top, now), leaf];
    assert!(in_force(&chain, now));
    assert!(!in_force(&chain, now + days(2)), "past its expiry");
}
