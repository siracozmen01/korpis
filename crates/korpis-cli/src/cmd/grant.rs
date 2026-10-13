// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis grant issue|revoke|list|show` (§7 of docs/13-surface-cli.md). Issuing to no named
//! holder creates a token that exists only to hold the grant (§3.7 of docs/08-identity.md),
//! which is how a pipeline gets a bounded, dated, attributable credential.

use super::{Answer, GrantAction, Issue, json, mutation, pairs, seconds};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use buffa_types::google::protobuf::Timestamp;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::subject::Kind as S;
use korpis_proto::korpis::v1::__buffa::oneof::{external_identity, list_grants_request, scope};
use std::fmt::Write as _;

fn local(message: String) -> CliError {
    CliError::Local(message)
}

/// The scope `text` names, its names resolved through what the caller can read.
async fn object_scope(c: &Connection, text: &str) -> Result<pb::Scope, CliError> {
    let (kind, named) = text.split_once(':').ok_or_else(|| {
        local(format!("{text:?}: a scope is kind:name, e.g. project:community-servers"))
    })?;
    let kind = match kind {
        "org" | "organization" => {
            scope::Kind::Organization(resolve::organization(c, Some(named)).await?)
        }
        "project" => scope::Kind::Project(resolve::project(c, named).await?),
        "workload" => scope::Kind::Workload(resolve::workload(c, named).await?),
        "node" => scope::Kind::Node(resolve::node(c, named).await?),
        "volume" => scope::Kind::Volume(named.to_owned()),
        "endpoint" => scope::Kind::Endpoint(named.to_owned()),
        "stream" => scope::Kind::Stream(named.to_owned()),
        other => return Err(local(format!("{other:?} is no scope kind"))),
    };
    Ok(pb::Scope { kind: Some(kind), ..Default::default() })
}

/// The scope of `issue`: its object, narrowed by `--select` when given.
async fn issue_scope(c: &Connection, i: &Issue) -> Result<pb::Scope, CliError> {
    let within = object_scope(c, &i.scope).await?;
    if i.select.is_empty() {
        return Ok(within);
    }
    let selector =
        pb::LabelSelector { match_labels: pairs("--select", &i.select)?, ..Default::default() };
    let kind = scope::Kind::Selector(Box::new(pb::SelectorScope {
        within: within.into(),
        selector: selector.into(),
        ..Default::default()
    }));
    Ok(pb::Scope { kind: Some(kind), ..Default::default() })
}

/// The subject `text` names.
fn subject(text: &str) -> Result<pb::Subject, CliError> {
    let external = |provider: &str, selector| {
        S::External(Box::new(pb::ExternalIdentity {
            provider: provider.to_owned(),
            selector: Some(selector),
            ..Default::default()
        }))
    };
    let parts: Vec<&str> = text.splitn(5, ':').collect();
    let kind = match parts.as_slice() {
        [id] if id.starts_with("usr_") => S::User((*id).to_owned()),
        [id] if id.starts_with("tok_") => S::Token((*id).to_owned()),
        [id] if id.starts_with("wl_") => S::Workload((*id).to_owned()),
        [id] if id.starts_with("ext_") => S::Extension((*id).to_owned()),
        ["ext", provider, "user", user] => {
            external(provider, external_identity::Selector::User((*user).to_owned()))
        }
        ["ext", provider, "set", realm, member] => {
            let set = pb::ExternalSet {
                realm: (*realm).to_owned(),
                member_of: (*member).to_owned(),
                ..Default::default()
            };
            external(provider, external_identity::Selector::Set(Box::new(set)))
        }
        _ => return Err(local(format!("{text:?} names no subject"))),
    };
    Ok(pb::Subject { kind: Some(kind), ..Default::default() })
}

/// A subject as a person reads it.
fn who(s: Option<&pb::Subject>) -> String {
    match s.and_then(|s| s.kind.as_ref()) {
        Some(S::User(id) | S::Token(id) | S::Workload(id) | S::Extension(id)) => id.clone(),
        Some(S::External(e)) => match &e.selector {
            Some(external_identity::Selector::User(u)) => format!("ext:{}:user:{u}", e.provider),
            Some(external_identity::Selector::Set(s)) => {
                format!("ext:{}:set:{}:{}", e.provider, s.realm, s.member_of)
            }
            None => "-".into(),
        },
        Some(S::System(a)) => short(a, "SYSTEM_ACTOR_"),
        None => "-".into(),
    }
}

/// A scope as a person reads it.
fn place(s: Option<&pb::Scope>) -> String {
    match s.and_then(|s| s.kind.as_ref()) {
        Some(
            scope::Kind::Organization(id)
            | scope::Kind::Project(id)
            | scope::Kind::Workload(id)
            | scope::Kind::Volume(id)
            | scope::Kind::Endpoint(id)
            | scope::Kind::Stream(id)
            | scope::Kind::Node(id),
        ) => id.clone(),
        Some(scope::Kind::Selector(s)) => {
            let labels = s.selector.as_option().map(|l| &l.match_labels);
            let pairs: Vec<String> =
                labels.into_iter().flatten().map(|(k, v)| format!("{k}={v}")).collect();
            format!("{} [{}]", place(s.within.as_option()), pairs.join(","))
        }
        None => "-".into(),
    }
}

fn rows(grants: &[pb::Grant]) -> String {
    let mut all = vec![
        ["ID", "LABEL", "HOLDER", "ACTIONS", "SCOPE", "STATE", "EXPIRES"]
            .map(str::to_owned)
            .to_vec(),
    ];
    all.extend(grants.iter().map(|g| {
        let expires = g.conditions.as_option().and_then(|c| c.expires_at.as_option());
        vec![
            g.id.clone(),
            g.label.clone(),
            who(g.subject.as_option()),
            g.actions.join(","),
            place(g.scope.as_option()),
            short(g.state, "GRANT_STATE_"),
            time(expires),
        ]
    }));
    table(&all)
}

/// The request `korpis grant issue` sends.
async fn issue_request(c: &Connection, i: &Issue) -> Result<pb::IssueGrantRequest, CliError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| local(e.to_string()))?;
    let now = i64::try_from(now.as_secs()).map_err(|e| local(e.to_string()))?;
    let expires_at = match &i.expires {
        Some(e) => {
            Some(Timestamp { seconds: now.saturating_add(seconds(e)?), ..Default::default() })
        }
        None => None,
    };
    let requires_approval_by = match &i.requires_approval_by {
        Some(s) => Some(object_scope(c, s).await?),
        None => None,
    };
    let conditions = pb::Conditions {
        expires_at: expires_at.into(),
        source_cidr: i.source_cidr.clone(),
        requires_approval_by: requires_approval_by.into(),
        ..Default::default()
    };
    let delegation = pb::DelegationPolicy {
        allowed: !i.delegate.is_empty(),
        max_depth: (!i.delegate.is_empty()).then_some(i.depth),
        actions: i.delegate.clone(),
        ..Default::default()
    };
    Ok(pb::IssueGrantRequest {
        mutation: mutation(None).into(),
        parent: i.parent.clone().unwrap_or_default(),
        subject: i.to.as_deref().map(subject).transpose()?.into(),
        actions: i.actions.clone(),
        scope: issue_scope(c, i).await?.into(),
        conditions: conditions.into(),
        delegation: delegation.into(),
        label: i.label.clone(),
        to_new_token: i.to.is_none(),
        ..Default::default()
    })
}

/// Answers `korpis grant issue`: the grant, and the new token's secret once.
async fn issue(c: &Connection, i: &Issue) -> Result<Answer, CliError> {
    let client = pb::GrantServiceClient::new(c.http(), c.config());
    let r = client.issue_grant(issue_request(c, i).await?).await?.into_owned();
    let mut human = rows(r.grant.as_option().map(std::slice::from_ref).unwrap_or_default());
    if !r.token.is_empty() {
        let _ = write!(
            human,
            "\ntoken, shown once and never again; use it as KORPIS_TOKEN:\n{}\n",
            r.token
        );
    }
    Ok(Answer { json: json(&r), human })
}

/// Answers `korpis grant …`.
pub async fn grant(c: &Connection, action: &GrantAction) -> Result<Answer, CliError> {
    let client = pb::GrantServiceClient::new(c.http(), c.config());
    match action {
        GrantAction::Issue(i) => issue(c, i).await,
        GrantAction::Revoke { grant } => {
            let request = pb::RevokeGrantRequest {
                mutation: mutation(None).into(),
                grant: grant.clone(),
                ..Default::default()
            };
            let r = client.revoke_grant(request).await?.into_owned();
            let g = r.grant.as_option().map(std::slice::from_ref).unwrap_or_default();
            let human = format!("{}revoked, with {} attenuated from it\n", rows(g), r.cascaded);
            Ok(Answer { json: json(&r), human })
        }
        GrantAction::List { scope, to } => {
            let filter = match (scope, to) {
                (Some(s), _) => {
                    Some(list_grants_request::Filter::Scope(Box::new(object_scope(c, s).await?)))
                }
                (None, Some(t)) => {
                    Some(list_grants_request::Filter::Subject(Box::new(subject(t)?)))
                }
                (None, None) => None,
            };
            let request = pb::ListGrantsRequest { filter, ..Default::default() };
            let list = client.list_grants(request).await?.into_owned().grants;
            Ok(Answer { human: rows(&list), json: json(&list) })
        }
        GrantAction::Show { grant } => {
            let request = pb::GetGrantRequest { id: grant.clone(), ..Default::default() };
            let r = client.get_grant(request).await?.into_owned();
            let mut all: Vec<pb::Grant> = r.grant.as_option().cloned().into_iter().collect();
            all.extend(r.chain.iter().cloned());
            Ok(Answer { human: rows(&all), json: json(&r) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subject_is_read_back_as_written() {
        for text in
            ["usr_1", "tok_1", "wl_1", "ext_1", "ext:discord:user:42", "ext:discord:set:456:mod"]
        {
            let s = subject(text).unwrap_or_default();
            assert_eq!(who(Some(&s)), text);
        }
        assert!(subject("prj_1").is_err());
        assert!(subject("extension:backup").is_err(), "an installation is named by its id");
        assert!(subject("ext:discord:role:1").is_err());
    }
}
