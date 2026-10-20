// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis identity bind|list|show|edit|retire|verify` (§5 and §5.1 of docs/08-identity.md):
//! identity bindings, through which a grant names a person or a set at an identity provider.
//! A binding is shown with its state, when its provider last confirmed it, and its interval and
//! grace together, since the longest an unreachable provider extends it is their sum.

use super::{Answer, json, mutation, seconds};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use buffa_types::google::protobuf::Duration;
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::external_identity::Selector;
use std::fmt::Write as _;

/// `korpis identity …`.
#[derive(Debug, Clone, Subcommand)]
pub enum IdentityAction {
    /// Binds a person or a set at an identity provider, so grants can name it. Takes
    /// `identity.binding.write` on the organization the provider is installed into. It is
    /// unverified until its provider first confirms it, within seconds.
    Bind {
        /// `user:<id>` for a person, `set:<realm>/<name>` for a set: a group claim, a role.
        subject: String,
        /// The identity provider, by its name in the organization.
        #[arg(long)]
        provider: String,
        /// The organization it is installed into; the top one you can read by default.
        #[arg(long)]
        org: Option<String>,
        /// For a set: the template its grants come from, as `tpl_…@<version>`.
        #[arg(long)]
        template: Option<String>,
        /// How often its provider is asked: a number then s, m, h, or d; 15m by default.
        #[arg(long)]
        interval: Option<String>,
        /// How long an unreachable provider extends it past its interval; 15m by default.
        #[arg(long)]
        grace: Option<String>,
        /// For a set: let its grants carry irreversible actions, deliberately (§5.2).
        #[arg(long)]
        irreversible: bool,
        /// For a set: another set, `<realm>/<name>`, its members must also be in.
        #[arg(long = "require")]
        require: Vec<String>,
    },
    /// The bindings declared at an organization; with `--retired`, retired ones too.
    List {
        /// The organization; the top one you can read by default.
        org: Option<String>,
        /// Retired bindings too.
        #[arg(long)]
        retired: bool,
    },
    /// One binding.
    Show {
        /// The binding, `idb_…`.
        binding: String,
    },
    /// Edits a binding's terms against the version it has now. An edit narrows at once and
    /// widens only for grants issued after it (§5).
    Edit {
        /// The binding, `idb_…`.
        binding: String,
        /// A new template, `tpl_…@<version>`.
        #[arg(long)]
        template: Option<String>,
        /// A new interval.
        #[arg(long)]
        interval: Option<String>,
        /// A new grace.
        #[arg(long)]
        grace: Option<String>,
        /// Whether its set's grants may carry irreversible actions.
        #[arg(long)]
        irreversible: Option<bool>,
    },
    /// Retires a binding: what is rooted in it is suspended, and its subject may be bound
    /// again.
    Retire {
        /// The binding, `idb_…`.
        binding: String,
    },
    /// Asks its provider about it now rather than at its next interval.
    Verify {
        /// The binding, `idb_…`.
        binding: String,
    },
}

fn local(message: String) -> CliError {
    CliError::Local(message)
}

fn span(text: &str) -> Result<Duration, CliError> {
    Ok(Duration { seconds: seconds(text)?, ..Default::default() })
}

fn template_ref(text: &str) -> Result<pb::TemplateRef, CliError> {
    let bad = || local(format!("{text:?}: a template as tpl_…@<version>"));
    let (id, version) = text.split_once('@').ok_or_else(bad)?;
    let version = version.parse().map_err(|_| bad())?;
    Ok(pb::TemplateRef { template: id.to_owned(), version, ..Default::default() })
}

fn set_of(text: &str) -> Result<pb::ExternalSet, CliError> {
    let (realm, member_of) =
        text.split_once('/').ok_or_else(|| local(format!("{text:?}: a set as <realm>/<name>")))?;
    Ok(pb::ExternalSet { realm: realm.into(), member_of: member_of.into(), ..Default::default() })
}

/// `user:<id>` or `set:<realm>/<name>`, at the provider `org/provider`.
fn subject(text: &str, provider: String) -> Result<pb::ExternalIdentity, CliError> {
    let selector = match text.split_once(':') {
        Some(("user", user)) if !user.is_empty() => Selector::User(user.into()),
        Some(("set", set)) => Selector::Set(Box::new(set_of(set)?)),
        _ => return Err(local(format!("{text:?}: user:<id> or set:<realm>/<name>"))),
    };
    Ok(pb::ExternalIdentity { provider, selector: Some(selector), ..Default::default() })
}

/// A subject as written: `user:<id>`, or `set:<realm>/<name>`.
pub(crate) fn named(s: Option<&pb::ExternalIdentity>) -> String {
    match s.and_then(|s| s.selector.as_ref()) {
        Some(Selector::User(u)) => format!("user:{u}"),
        Some(Selector::Set(set)) => format!("set:{}/{}", set.realm, set.member_of),
        None => "-".into(),
    }
}

fn duration(d: Option<&Duration>) -> String {
    match d.map(|d| d.seconds) {
        Some(s) if s % 3600 == 0 => format!("{}h", s / 3600),
        Some(s) if s % 60 == 0 => format!("{}m", s / 60),
        Some(s) => format!("{s}s"),
        None => "-".into(),
    }
}

fn state(b: &pb::IdentityBinding) -> String {
    if b.retired_at.is_set() {
        return "retired".into();
    }
    b.state.as_known().map_or_else(|| "-".into(), |s| short(s, "BINDING_STATE_"))
}

fn shown(b: &pb::IdentityBinding) -> String {
    let grace = b.constraints.as_option().and_then(|c| c.grace.as_option());
    let interval = b.reverify_interval.as_option();
    let longest = interval
        .zip(grace)
        .map(|(i, g)| Duration { seconds: i.seconds + g.seconds, ..Default::default() });
    let template = b
        .template
        .as_option()
        .map_or_else(|| "none".into(), |t| format!("{}@{}", t.template, t.version));
    let requires: Vec<String> = b
        .constraints
        .as_option()
        .map(|c| {
            c.require_membership.iter().map(|s| format!("{}/{}", s.realm, s.member_of)).collect()
        })
        .unwrap_or_default();
    format!(
        "{id}  {subject} at {org}/{provider}\n  {state}, last confirmed {last}\n  asked every \
         {interval}, with {grace} of grace: an unreachable provider extends it at most {longest} \
         past its last confirmation\n  template {template}; irreversible actions {irreversible}; \
         members must also be in {requires}\n  made by {by} at {at}, version {version}\n",
        id = b.id,
        subject = named(b.subject.as_option()),
        org = b.organization,
        provider = b.provider,
        state = state(b),
        last = time(b.last_verified_at.as_option()),
        interval = duration(interval),
        grace = duration(grace),
        longest = duration(longest.as_ref()),
        irreversible = if b.allows_irreversible { "allowed" } else { "not allowed" },
        requires = if requires.is_empty() { "nothing more".into() } else { requires.join(", ") },
        by = super::grant::who(b.made_by.as_option()),
        at = time(b.made_at.as_option()),
        version = b.version,
    )
}

fn rows(list: &[pb::IdentityBinding]) -> String {
    let mut out = vec![
        ["BINDING", "SUBJECT", "PROVIDER", "STATE", "LAST CONFIRMED", "INTERVAL"]
            .map(String::from)
            .to_vec(),
    ];
    for b in list {
        out.push(vec![
            b.id.clone(),
            named(b.subject.as_option()),
            b.provider.clone(),
            state(b),
            time(b.last_verified_at.as_option()),
            duration(b.reverify_interval.as_option()),
        ]);
    }
    table(&out)
}

/// The terms a binding has now, so an edit changes only what it names.
fn terms_of(b: &pb::IdentityBinding) -> pb::IdentityBindingTerms {
    pb::IdentityBindingTerms {
        template: b.template.clone(),
        constraints: b.constraints.clone(),
        reverify_interval: b.reverify_interval.clone(),
        allows_irreversible: b.allows_irreversible,
        ..Default::default()
    }
}

/// What `korpis identity bind` declares.
struct Asked<'a> {
    template: Option<&'a str>,
    interval: Option<&'a str>,
    grace: Option<&'a str>,
    irreversible: Option<bool>,
    require: &'a [String],
}

/// `terms` with what `asked` names changed.
fn changed(
    mut terms: pb::IdentityBindingTerms,
    asked: &Asked<'_>,
) -> Result<pb::IdentityBindingTerms, CliError> {
    if let Some(t) = asked.template {
        terms.template = Some(template_ref(t)?).into();
    }
    if let Some(i) = asked.interval {
        terms.reverify_interval = Some(span(i)?).into();
    }
    if let Some(irreversible) = asked.irreversible {
        terms.allows_irreversible = irreversible;
    }
    let constraints = terms.constraints.get_or_insert_default();
    if let Some(g) = asked.grace {
        constraints.grace = Some(span(g)?).into();
    }
    if !asked.require.is_empty() {
        constraints.require_membership =
            asked.require.iter().map(|s| set_of(s)).collect::<Result<_, _>>()?;
    }
    Ok(terms)
}

async fn get(c: &Connection, id: &str) -> Result<pb::IdentityBinding, CliError> {
    let client = pb::IdentityServiceClient::new(c.http(), c.config());
    let req = pb::GetIdentityBindingRequest { id: id.to_owned(), ..Default::default() };
    let got = client.get_identity_binding(req).await?.into_owned();
    got.binding.into_option().ok_or_else(|| local(format!("no binding {id}")))
}

async fn bindings(
    c: &Connection,
    org: String,
    retired: bool,
) -> Result<Vec<pb::IdentityBinding>, CliError> {
    let client = pb::IdentityServiceClient::new(c.http(), c.config());
    let (mut all, mut page_token) = (Vec::new(), String::new());
    loop {
        let request = pb::ListIdentityBindingsRequest {
            organization: org.clone(),
            retired,
            page: pb::PageRequest { page_token, ..Default::default() }.into(),
            ..Default::default()
        };
        let listed = client.list_identity_bindings(request).await?.into_owned();
        all.extend(listed.bindings);
        page_token = listed.page.as_option().map(|p| p.next_page_token.clone()).unwrap_or_default();
        if page_token.is_empty() {
            return Ok(all);
        }
    }
}

fn one(b: Option<&pb::IdentityBinding>) -> Answer {
    Answer { human: b.map(shown).unwrap_or_default(), json: json(&b) }
}

async fn bind(c: &Connection, a: &IdentityAction) -> Result<Answer, CliError> {
    let IdentityAction::Bind {
        subject: text,
        provider,
        org,
        template,
        interval,
        grace,
        irreversible,
        require,
    } = a
    else {
        return Err(local("not a bind".into()));
    };
    let organization = resolve::organization(c, org.as_deref()).await?;
    let asked = Asked {
        template: template.as_deref(),
        interval: interval.as_deref(),
        grace: grace.as_deref(),
        irreversible: irreversible.then_some(true),
        require,
    };
    let request = pb::CreateIdentityBindingRequest {
        mutation: mutation(None).into(),
        subject: subject(text, format!("{organization}/{provider}"))?.into(),
        organization,
        provider: provider.clone(),
        terms: changed(pb::IdentityBindingTerms::default(), &asked)?.into(),
        ..Default::default()
    };
    let client = pb::IdentityServiceClient::new(c.http(), c.config());
    let made = client.create_identity_binding(request).await?.into_owned();
    Ok(one(made.binding.as_option()))
}

/// Answers `korpis identity …`.
pub async fn identity(c: &Connection, a: &IdentityAction) -> Result<Answer, CliError> {
    let client = pb::IdentityServiceClient::new(c.http(), c.config());
    match a {
        IdentityAction::Bind { .. } => bind(c, a).await,
        IdentityAction::List { org, retired } => {
            let list =
                bindings(c, resolve::organization(c, org.as_deref()).await?, *retired).await?;
            Ok(Answer { human: rows(&list), json: json(&list) })
        }
        IdentityAction::Show { binding } => Ok(one(Some(&get(c, binding).await?))),
        IdentityAction::Edit { binding, template, interval, grace, irreversible } => {
            let now = get(c, binding).await?;
            let asked = Asked {
                template: template.as_deref(),
                interval: interval.as_deref(),
                grace: grace.as_deref(),
                irreversible: *irreversible,
                require: &[],
            };
            let request = pb::UpdateIdentityBindingRequest {
                mutation: mutation(Some(now.version)).into(),
                id: binding.clone(),
                terms: changed(terms_of(&now), &asked)?.into(),
                ..Default::default()
            };
            let edited = client.update_identity_binding(request).await?.into_owned();
            Ok(one(edited.binding.as_option()))
        }
        IdentityAction::Retire { binding } => {
            let request = pb::RetireIdentityBindingRequest {
                mutation: mutation(None).into(),
                id: binding.clone(),
                ..Default::default()
            };
            let retired = client.retire_identity_binding(request).await?.into_owned();
            Ok(one(retired.binding.as_option()))
        }
        IdentityAction::Verify { binding } => {
            let request =
                pb::VerifyIdentityBindingRequest { id: binding.clone(), ..Default::default() };
            let asked = client.verify_identity_binding(request).await?.into_owned();
            let mut answer = one(asked.binding.as_option());
            if !asked.unanswered.is_empty() {
                let _ =
                    writeln!(answer.human, "  the provider did not answer: {}", asked.unanswered);
            }
            Ok(answer)
        }
    }
}
