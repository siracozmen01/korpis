// SPDX-License-Identifier: AGPL-3.0-or-later

//! What issuing a grant writes beside it: the account and invitation a grant to a new user
//! creates (§9.6 of docs/08-identity.md), and the effect that records the issue.

use crate::caller::Caller;
use crate::error::AppError;
use korpis_core::account::record::{Account, INVITATION_LIFETIME, Invitation, display_name};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::GrantRecord;
use korpis_core::authz::credential::{Secret, SecretKind};
use std::time::SystemTime;
use uuid::Uuid;

/// The account and invitation a grant to a new user creates, issued by `caller` at `now`.
pub(super) fn new_user(
    caller: &Caller,
    name: &str,
    expires_at: Option<SystemTime>,
    now: SystemTime,
) -> Result<(Account, Invitation, Secret), AppError> {
    let latest = now + INVITATION_LIFETIME;
    let expires_at = expires_at.unwrap_or(latest);
    if expires_at <= now || expires_at > latest {
        return Err(AppError::invalid("invitation_expires_at", "within the next seven days"));
    }
    let account = Account {
        id: Uuid::now_v7(),
        display_name: display_name(name)
            .map_err(|e| AppError::invalid("display_name", e.to_string()))?,
        name: None,
        created_by: caller.principal.key(),
        created_at: now,
        disabled_at: None,
    };
    let invitation = Invitation {
        id: Uuid::now_v7(),
        account: account.id,
        issued_by: caller.principal.key(),
        expires_at,
        passkey_only: false,
        redeemed_at: None,
    };
    let secret = Secret::generate_kind(SecretKind::Invitation)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok((account, invitation, secret))
}

/// The effect of `caller` issuing `record` beneath `parent`, about the scope it was issued over.
pub(super) fn issued(caller: &Caller, record: &GrantRecord, parent: Uuid) -> Effect {
    caller
        .effect("grant.issued", Outcome::Succeeded)
        .under(parent)
        .about(record.grant.scope.anchor())
        .after(details([
            ("grant", record.grant.id.to_string().into()),
            ("parent", parent.to_string().into()),
            ("holder", record.holder.key().into()),
            ("label", record.label.clone().into()),
            ("template", record.from_template.map(|t| t.template.to_string()).into()),
            ("template_version", record.from_template.map(|t| t.version).into()),
        ]))
}
