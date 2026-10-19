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
    /// Who asks.
    pub subject: &'a Subject,
    /// What they ask to do.
    pub action: &'a Action,
    /// What they ask to do it to, with its ancestry.
    pub resource: &'a Resource,
    /// Where the request came from, for network conditions.
    pub source_ip: Option<IpAddr>,
    /// Whether the credential was presented with a second factor.
    pub mfa: bool,
    /// When; conditions are evaluated at this instant.
    pub now: SystemTime,
    /// Whether this renews a session token whose use was spent when the first was minted
    /// (§6.1 of docs/08-identity.md): the use cap is then neither consulted nor spent again.
    pub renewal: bool,
}

/// The outcome of evaluation.
///
/// Proposing is a separate variant rather than a flag on a permit, so that no caller can let a
/// propose-only grant through by forgetting to read a field: an action that is not a
/// declaration (a console keystroke, a file read) must treat [`Decision::Propose`] as a refusal,
/// and the compiler makes every caller say what it does with one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The request may be performed now.
    Permit(Permit),
    /// The grant can propose, not apply (§3.6): a declaration under it produces a plan that waits
    /// for an approver over `approval_scope`, and any other change is refused. An action that
    /// changes nothing is permitted under it instead.
    Propose {
        /// The permit the action will run under once approved.
        permit: Permit,
        /// Where an approver must hold `plan.approve`.
        approval_scope: Scope,
    },
    /// Refused.
    Deny(Denial),
}

/// The authority a permitted or proposed request acts under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permit {
    /// The grant that authorized the request, which every effect names.
    pub grant: Uuid,
    /// The chain, leaf first, root last.
    pub chain: Vec<Uuid>,
    /// The leaf has a use limit: the gateway must redeem one use atomically, in the transaction
    /// that performs the action (§6.2).
    pub redeem: bool,
    /// The leaf's `source_cidr`, which attenuation keeps within every ancestor's (§3.2), so a
    /// token minted from this chain carries it for the relay to check (§6.2).
    pub source_cidr: Vec<ipnet::IpNet>,
    /// The leaf's stream range, which attenuation keeps within every ancestor's: whatever reads
    /// the stream under this permit reads only that range (§8 of docs/14-streams.md).
    pub stream_range: Option<super::grant::StreamRange>,
}

/// Why a request was refused. The caller is told which action was missing, never which grant
/// would have granted it (§8 of docs/10-api.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    /// The action no chain carried, which is all the caller is told.
    pub missing_action: Action,
    /// For the audit log and the operator, never for the caller.
    pub reasons: Vec<(Uuid, ChainError)>,
}

/// Why one candidate chain did not permit the request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    /// A grant names a parent that was not supplied.
    #[error("the chain is incomplete: grant {0} names a parent that was not supplied")]
    BrokenChain(Uuid),
    /// A parentless grant that is not a root grant.
    #[error("grant {0} has no parent and is not a root grant")]
    NotRooted(Uuid),
    /// A grant on the chain is revoked.
    #[error("grant {0} is revoked")]
    Revoked(Uuid),
    /// A grant on the chain is suspended.
    #[error("grant {0} is suspended")]
    Suspended(Uuid),
    /// A grant on the chain is held by an external identity no live, verified binding vouches
    /// for: suspended by derivation, reversibly (§5.1 of docs/08-identity.md).
    #[error("grant {0} is held by an external identity nothing vouches for now")]
    Unvouched(Uuid),
    /// A grant on the chain is outside its validity window.
    #[error("grant {0} is not in force")]
    NotInForce(Uuid),
    /// A grant on the chain is not a valid attenuation of its parent.
    #[error("grant {0} is not a valid attenuation of its parent: {1}")]
    InvalidAttenuation(Uuid, super::grant::AttenuationError),
    /// No grants were supplied.
    #[error("no grants were supplied")]
    EmptyChain,
    /// The leaf is held by someone else.
    #[error("the grant is held by a different subject")]
    WrongSubject,
    /// The leaf does not carry the action.
    #[error("the grant does not carry the action")]
    ActionNotHeld,
    /// The resource is outside the leaf's scope.
    #[error("the resource is outside the grant's scope")]
    OutOfScope,
    /// A private action would cross into another organization implicitly.
    #[error("a private action does not reach into another organization implicitly")]
    PrivateAcrossOrganization,
    /// The grant's scope lies in a suspended organization, and the action is not one a
    /// suspended tenant keeps (§2.1 of docs/08-identity.md).
    #[error("the grant's organization is suspended")]
    OrganizationSuspended,
    /// The request's source is outside a network condition.
    #[error("the request's source address is outside the grant's networks")]
    SourceNotAllowed,
    /// A condition requires a second factor the request lacks.
    #[error("the grant requires MFA")]
    MfaRequired,
    /// A use-limited grant has no uses left.
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
                if matches!(&decision, Decision::Permit(p) if !p.redeem) {
                    return decision;
                }
                // Prefer a direct permit over a proposal, and either over a redemption.
                let better = matches!(
                    (&best, &decision),
                    (None, _) | (Some(Decision::Propose { .. }), Decision::Permit(_))
                );
                if better {
                    best = Some(decision);
                }
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
    let leaf = chain.first().ok_or(ChainError::EmptyChain)?;
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
    // Suspension is derived from the scope's ancestry and never written onto a grant: a grant
    // scoped in a suspended organization's subtree is suspended, and every chain beneath it is
    // too, since each child's scope lies within its parent's (§2.1, §3.5).
    let suspended = hierarchy.resource(leaf.scope.anchor()).is_some_and(|r| r.suspended());
    if suspended && !request.action.stays_while_suspended() {
        return Err(ChainError::OrganizationSuspended);
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
    if leaf.remaining_uses() == Some(0) && !request.renewal {
        return Err(ChainError::Exhausted);
    }
    let permit = Permit {
        grant: leaf.id,
        chain: chain.iter().map(|g| g.id).collect(),
        redeem: leaf.conditions.max_uses.is_some() && !request.renewal,
        source_cidr: leaf.conditions.source_cidr.clone(),
        stream_range: leaf.conditions.stream_range,
    };
    // A propose-only grant proposes changes; reading under it is not a change (§3.6).
    Ok(match &leaf.conditions.requires_approval_by {
        Some(scope) if !request.action.changes_nothing() => {
            Decision::Propose { permit, approval_scope: scope.clone() }
        }
        _ => Decision::Permit(permit),
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
        let narrowed = super::binding::vouched_form(link, hierarchy, now)?;
        let link = narrowed.as_ref().unwrap_or(link);
        match (link.parent, chain.get(i + 1)) {
            // Only a grant created as a root (at installation, or by local root recovery) may
            // end a chain; a parentless grant that is not marked root roots nothing.
            (None, None) if link.root => {}
            (Some(p), Some(parent)) if parent.id == p => {
                check_attenuation(parent, link, hierarchy, At::Use)
                    .map_err(|e| ChainError::InvalidAttenuation(link.id, e))?;
            }
            (None, None) => return Err(ChainError::NotRooted(link.id)),
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
    /// Who would approve.
    pub approver: &'a Subject,
    /// The approver's candidate chains, each leaf first.
    pub approver_chains: &'a [Vec<Grant>],
    /// Who proposed.
    pub proposer: &'a Subject,
    /// The chain the plan was proposed under, leaf first.
    pub proposing_chain: &'a [Grant],
    /// The scope the proposing grant requires approval over.
    pub approval_scope: &'a Scope,
    /// Where the approval came from.
    pub source_ip: Option<IpAddr>,
    /// Whether the approver presented a second factor.
    pub mfa: bool,
    /// When.
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
        renewal: false,
    };
    match evaluate(&request, &eligible, hierarchy) {
        Decision::Propose { .. } => deny(Vec::new()),
        decision => decision,
    }
}

/// Why a grant could not be issued.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IssueError {
    /// The issuer does not hold the parent.
    #[error("the issuer does not hold the parent grant")]
    NotHolder,
    /// The parent's own chain is not valid now.
    #[error("the parent chain is not valid now: {0}")]
    ParentChain(ChainError),
    /// The override was claimed by a request rather than granted by an identity binding.
    #[error("only an operator's identity binding can override the irreversible-action default")]
    OverrideNotFromBinding,
    /// A root grant is created by installation, never issued.
    #[error("a root grant is created at installation, never issued")]
    RootNotIssuable,
    /// The child is not a valid attenuation of the parent.
    #[error(transparent)]
    Attenuation(#[from] super::grant::AttenuationError),
}

/// Issues `child` from the first grant of `parent_chain` on behalf of `issuer`. Issuing a grant to
/// someone *is* attenuating a grant one holds (§3.2 of docs/08-identity.md), so the issuer must be
/// the parent's subject, the parent's whole chain must be valid now, and the child must be a valid
/// attenuation of it, use limits included. The gateway runs this before inserting the child, in
/// the transaction that takes the parent's remaining uses (§6.2).
///
/// `binding_allows_irreversible` is what the subject's identity binding says, read by the caller
/// from the binding an operator declared (§5.2), never from the request: a child carrying
/// `irreversible_override` is refused unless it is a set subject whose binding allows it.
pub fn issue(
    issuer: &Subject,
    parent_chain: &[Grant],
    child: &Grant,
    binding_allows_irreversible: bool,
    hierarchy: &impl Hierarchy,
    now: SystemTime,
) -> Result<(), IssueError> {
    if child.root {
        return Err(IssueError::RootNotIssuable);
    }
    if child.irreversible_override && !(child.subject.set && binding_allows_irreversible) {
        return Err(IssueError::OverrideNotFromBinding);
    }
    let Some(parent) = parent_chain.first() else { return Err(IssueError::NotHolder) };
    if &parent.subject != issuer || child.parent != Some(parent.id) {
        return Err(IssueError::NotHolder);
    }
    verify_chain(parent_chain, hierarchy, now).map_err(IssueError::ParentChain)?;
    check_attenuation(parent, child, hierarchy, At::Issue)?;
    Ok(())
}
