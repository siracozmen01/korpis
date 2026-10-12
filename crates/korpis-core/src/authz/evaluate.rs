// SPDX-License-Identifier: AGPL-3.0-or-later

//! Use-time evaluation: is this request permitted by one of the caller's grant chains?
//!
//! A grant is not a certificate that keeps working once printed; it is a claim re-derived at
//! every use (§3.2 of docs/08-identity.md). So evaluation walks the whole chain, leaf to root,
//! and re-checks every link: not revoked, not suspended, in force, and a valid attenuation of
//! its parent. Revocation is therefore derived, never written to a subtree (§3.5).

use super::action::Action;
use super::grant::{At, Grant, Subject, check_attenuation, in_force};
use super::scope::{Hierarchy, Resource, Scope};
use std::net::IpAddr;
use std::time::SystemTime;
use uuid::Uuid;

/// What is being asked.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub subject: &'a Subject,
    pub action: &'a Action,
    pub resource: &'a Resource,
    pub source_ip: Option<IpAddr>,
    pub mfa: bool,
    pub now: SystemTime,
}

/// The outcome of evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Permit {
        /// The grant that authorized the request, which every effect names.
        grant: Uuid,
        /// The chain, leaf first, root last.
        chain: Vec<Uuid>,
        /// The leaf has a use limit: the gateway must redeem one use atomically, in the
        /// transaction that performs the action (§6.2).
        redeem: bool,
        /// The grant can propose, not apply: plans wait for an approver over this scope (§3.6).
        propose_only: Option<Scope>,
    },
    Deny(Denial),
}

/// Why a request was refused. The caller is told which action was missing, never which grant
/// would have granted it (§8 of docs/10-api.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    pub missing_action: Action,
    /// For the audit log and the operator, never for the caller.
    pub reasons: Vec<(Uuid, ChainError)>,
}

/// Why one candidate chain did not permit the request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    #[error("the chain is incomplete: grant {0} names a parent that was not supplied")]
    BrokenChain(Uuid),
    #[error("grant {0} is revoked")]
    Revoked(Uuid),
    #[error("grant {0} is suspended")]
    Suspended(Uuid),
    #[error("grant {0} is not in force")]
    NotInForce(Uuid),
    #[error("grant {0} is not a valid attenuation of its parent: {1}")]
    InvalidAttenuation(Uuid, super::grant::AttenuationError),
    #[error("the grant is held by a different subject")]
    WrongSubject,
    #[error("the grant does not carry the action")]
    ActionNotHeld,
    #[error("the resource is outside the grant's scope")]
    OutOfScope,
    #[error("a private action does not reach into another organization implicitly")]
    PrivateAcrossOrganization,
    #[error("the request's source address is outside the grant's networks")]
    SourceNotAllowed,
    #[error("the grant requires MFA")]
    MfaRequired,
    #[error("the grant's uses are exhausted")]
    Exhausted,
}

/// Evaluates `request` against candidate chains, each leaf first and root last. Returns the
/// first chain that permits, preferring one that needs neither approval nor redemption.
pub fn evaluate(
    request: &Request<'_>,
    chains: &[Vec<Grant>],
    hierarchy: &impl Hierarchy,
) -> Decision {
    let mut reasons = Vec::new();
    let mut best: Option<Decision> = None;
    for chain in chains {
        let Some(leaf) = chain.first() else { continue };
        match permits(request, chain, hierarchy) {
            Ok(decision) => {
                let Decision::Permit { redeem, propose_only, .. } = &decision else {
                    unreachable!()
                };
                if !redeem && propose_only.is_none() {
                    return decision;
                }
                best.get_or_insert(decision);
            }
            Err(e) => reasons.push((leaf.id, e)),
        }
    }
    best.unwrap_or_else(|| {
        Decision::Deny(Denial { missing_action: request.action.clone(), reasons })
    })
}

fn permits(
    request: &Request<'_>,
    chain: &[Grant],
    hierarchy: &impl Hierarchy,
) -> Result<Decision, ChainError> {
    verify_chain(chain, hierarchy, request.now)?;
    let leaf = &chain[0];
    if &leaf.subject != request.subject {
        return Err(ChainError::WrongSubject);
    }
    if !leaf.actions.contains(request.action) {
        return Err(ChainError::ActionNotHeld);
    }
    if !request.resource.is_within(&leaf.scope) {
        return Err(ChainError::OutOfScope);
    }
    if request.action.is_private() && !private_reach(&leaf.scope, request.resource, hierarchy) {
        return Err(ChainError::PrivateAcrossOrganization);
    }
    for link in chain {
        let nets = &link.conditions.source_cidr;
        if !nets.is_empty()
            && !request.source_ip.is_some_and(|ip| nets.iter().any(|n| n.contains(&ip)))
        {
            return Err(ChainError::SourceNotAllowed);
        }
        if link.conditions.requires_mfa && !request.mfa {
            return Err(ChainError::MfaRequired);
        }
    }
    if leaf.remaining_uses() == Some(0) {
        return Err(ChainError::Exhausted);
    }
    Ok(Decision::Permit {
        grant: leaf.id,
        chain: chain.iter().map(|g| g.id).collect(),
        redeem: leaf.conditions.max_uses.is_some(),
        propose_only: leaf.conditions.requires_approval_by.clone(),
    })
}

/// Re-derives the validity of a whole chain at `now`.
pub fn verify_chain(
    chain: &[Grant],
    hierarchy: &impl Hierarchy,
    now: SystemTime,
) -> Result<(), ChainError> {
    for (i, link) in chain.iter().enumerate() {
        if link.revoked {
            return Err(ChainError::Revoked(link.id));
        }
        if link.suspended {
            return Err(ChainError::Suspended(link.id));
        }
        if !in_force(&link.conditions, now) {
            return Err(ChainError::NotInForce(link.id));
        }
        match (link.parent, chain.get(i + 1)) {
            (None, None) => {}
            (Some(p), Some(parent)) if parent.id == p => {
                check_attenuation(parent, link, hierarchy, At::Use)
                    .map_err(|e| ChainError::InvalidAttenuation(link.id, e))?;
            }
            _ => return Err(ChainError::BrokenChain(link.id)),
        }
    }
    Ok(())
}

/// For a private action on a resource owned by organization O, the permitting grant's scope
/// must be O or contained in O (§3.3 of docs/08-identity.md). A grant scoped to a strict
/// ancestor of O does not reach O's private data, however broad it is.
fn private_reach(scope: &Scope, resource: &Resource, hierarchy: &impl Hierarchy) -> bool {
    let Some(owner) = resource.owning_organization() else { return false };
    let Some(anchor) = hierarchy.resource(scope.anchor()) else { return false };
    anchor.owning_organization() == Some(owner)
}

/// An approval being attempted (§3.6 of docs/08-identity.md).
#[derive(Debug, Clone)]
pub struct Approval<'a> {
    pub approver: &'a Subject,
    /// The approver's candidate chains, each leaf first.
    pub approver_chains: &'a [Vec<Grant>],
    pub proposer: &'a Subject,
    /// The chain the plan was proposed under, leaf first.
    pub proposing_chain: &'a [Grant],
    /// The scope the proposing grant requires approval over.
    pub approval_scope: &'a Scope,
    pub source_ip: Option<IpAddr>,
    pub mfa: bool,
    pub now: SystemTime,
}

/// Decides an approval. It goes through [`evaluate`] like every other action, so every
/// condition on the approver's chain binds (revocation, expiry, networks, MFA, use limits), and
/// then adds what is particular to approval:
///
/// - the approver holds `plan.approve` through a grant whose scope contains the approval scope,
///   so the set of approvers shrinks as the scope grows;
/// - the approver's own grant is not propose-only: an approval cannot itself await approval;
/// - nobody approves their own plan: the approver is a different subject, and the approving
///   chain does not pass through the proposing grant, so minting oneself a child grant does not
///   manufacture a second approver. Ancestors the two chains share (the root, at least) are not
///   the proposing grant and do not disqualify anyone.
pub fn may_approve(approval: &Approval<'_>, hierarchy: &impl Hierarchy) -> Decision {
    let approve = Action::parse("plan.approve").unwrap_or_else(|_| unreachable!("core action"));
    let deny = |reasons| Decision::Deny(Denial { missing_action: approve.clone(), reasons });
    if approval.approver == approval.proposer {
        return deny(Vec::new());
    }
    let Some(anchor) = hierarchy.resource(approval.approval_scope.anchor()) else {
        return deny(Vec::new());
    };
    let proposing = approval.proposing_chain.first().map(|g| g.id);
    let eligible: Vec<Vec<Grant>> = approval
        .approver_chains
        .iter()
        .filter(|chain| {
            chain.first().is_some_and(|leaf| {
                super::scope::scope_within(approval.approval_scope, &leaf.scope, hierarchy)
            }) && !chain.iter().any(|g| Some(g.id) == proposing)
        })
        .cloned()
        .collect();
    let request = Request {
        subject: approval.approver,
        action: &approve,
        resource: &anchor,
        source_ip: approval.source_ip,
        mfa: approval.mfa,
        now: approval.now,
    };
    match evaluate(&request, &eligible, hierarchy) {
        Decision::Permit { propose_only: Some(_), .. } => deny(Vec::new()),
        decision => decision,
    }
}

/// Why a grant could not be issued.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IssueError {
    #[error("the issuer does not hold the parent grant")]
    NotHolder,
    #[error("the parent chain is not valid now: {0}")]
    ParentChain(ChainError),
    #[error(transparent)]
    Attenuation(#[from] super::grant::AttenuationError),
}

/// Issues `child` from the first grant of `parent_chain` on behalf of `issuer`. Issuing a grant to
/// someone *is* attenuating a grant one holds (§3.2 of docs/08-identity.md), so the issuer must be
/// the parent's subject, the parent's whole chain must be valid now, and the child must be a valid
/// attenuation of it, use limits included. The gateway runs this before inserting the child, in
/// the transaction that takes the parent's remaining uses (§6.2).
pub fn issue(
    issuer: &Subject,
    parent_chain: &[Grant],
    child: &Grant,
    hierarchy: &impl Hierarchy,
    now: SystemTime,
) -> Result<(), IssueError> {
    let Some(parent) = parent_chain.first() else { return Err(IssueError::NotHolder) };
    if &parent.subject != issuer || child.parent != Some(parent.id) {
        return Err(IssueError::NotHolder);
    }
    verify_chain(parent_chain, hierarchy, now).map_err(IssueError::ParentChain)?;
    check_attenuation(parent, child, hierarchy, At::Issue)?;
    Ok(())
}
