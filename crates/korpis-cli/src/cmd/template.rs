// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis template create|list|show|edit|retire|holders` (§4 of docs/08-identity.md): grant
//! templates, the roles of the interface. A template is written as a file holding what
//! `template show --json` prints, so a template is read, edited, and written back; an edit is
//! made against the version the file says it was read at, and a template edited since is a
//! conflict. `holders` finds the grants issued from a template and marks those behind it.

use super::grant::{self, who};
use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::{list_grant_templates_request, list_grants_request};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// `korpis template …`.
#[derive(Debug, Clone, Subcommand)]
pub enum TemplateAction {
    /// Makes a template owned by an organization, from a file: JSON or YAML, as `template show
    /// --json` prints one. Takes `template.write` on the organization.
    Create {
        /// The organization that owns it, offering it to everything beneath it.
        organization: String,
        /// The template.
        #[arg(short = 'f', long = "file")]
        file: PathBuf,
    },
    /// The templates offered at an organization, owned by it or by one above it; with
    /// `--owned`, those it owns, retired ones too.
    List {
        /// The organization; the top one you can read by default.
        organization: Option<String>,
        /// Only those it owns, retired ones included.
        #[arg(long)]
        owned: bool,
    },
    /// A template, or one of its versions.
    Show {
        /// The template: `tpl_…`, or a name offered at `--org`.
        template: String,
        /// A version it had; its current one by default.
        #[arg(long)]
        version: Option<u64>,
        /// The organization it is read at: where it is offered, which names resolve at.
        #[arg(long)]
        org: Option<String>,
    },
    /// Edits a template from a file holding what `template show --json` printed, against the
    /// version it printed: its next version. Takes `template.write` on its organization.
    Edit {
        /// The template: `tpl_…`, or a name offered at `--org`.
        template: String,
        /// The template as edited.
        #[arg(short = 'f', long = "file")]
        file: PathBuf,
        /// The organization names resolve at.
        #[arg(long)]
        org: Option<String>,
    },
    /// Retires a template: offered nowhere since, kept because grants name its versions.
    Retire {
        /// The template: `tpl_…`, or a name offered at `--org`.
        template: String,
        /// The organization names resolve at.
        #[arg(long)]
        org: Option<String>,
    },
    /// The grants issued from a template that you may read, at the version each came from;
    /// those behind its current version are marked.
    Holders {
        /// The template: `tpl_…`, or a name offered at `--org`.
        template: String,
        /// The organization names resolve at.
        #[arg(long)]
        org: Option<String>,
    },
}

fn local(message: String) -> CliError {
    CliError::Local(message)
}

/// An organization as the object a template is read at.
fn at(org: &str) -> pb::ObjectRef {
    pb::ObjectRef {
        kind: pb::ObjectKind::OBJECT_KIND_ORGANIZATION.into(),
        id: org.to_owned(),
        ..Default::default()
    }
}

/// The templates offered at `at`, every page of them, or owned by an organization.
pub(super) async fn templates(
    c: &Connection,
    of: list_grant_templates_request::Of,
) -> Result<Vec<pb::GrantTemplate>, CliError> {
    let client = pb::GrantServiceClient::new(c.http(), c.config());
    let mut all = Vec::new();
    let mut page_token = String::new();
    loop {
        let request = pb::ListGrantTemplatesRequest {
            of: Some(of.clone()),
            page: pb::PageRequest { page_token, ..Default::default() }.into(),
            ..Default::default()
        };
        let listed = client.list_grant_templates(request).await?.into_owned();
        all.extend(listed.templates);
        page_token = listed.page.as_option().map(|p| p.next_page_token.clone()).unwrap_or_default();
        if page_token.is_empty() {
            return Ok(all);
        }
    }
}

/// The template `text` names, its identifier as written or a name among those offered at
/// `place`; and the object it is read at, when one was named.
pub(super) async fn named(
    c: &Connection,
    text: &str,
    place: Option<pb::ObjectRef>,
) -> Result<(String, Option<pb::ObjectRef>), CliError> {
    if text.starts_with("tpl_") {
        return Ok((text.to_owned(), place));
    }
    let Some(place) = place else {
        return Err(local(format!("{text:?}: name a template by tpl_…, or say where with --org")));
    };
    let offered = templates(c, list_grant_templates_request::Of::At(Box::new(place.clone())));
    let matching: Vec<pb::GrantTemplate> =
        offered.await?.into_iter().filter(|t| t.name == text).collect();
    match matching.as_slice() {
        [one] => Ok((one.id.clone(), Some(place))),
        [] => Err(local(format!("no template {text:?} is offered there"))),
        _ => Err(local(format!("{text:?} names more than one template there; use its tpl_ id"))),
    }
}

/// The object `--org` names, if it was given.
async fn org_at(c: &Connection, org: Option<&str>) -> Result<Option<pb::ObjectRef>, CliError> {
    match org {
        Some(o) => Ok(Some(at(&resolve::organization(c, Some(o)).await?))),
        None => Ok(None),
    }
}

/// A template, read at `place` when given.
pub(super) async fn read(
    c: &Connection,
    id: &str,
    version: Option<u64>,
    place: Option<pb::ObjectRef>,
) -> Result<pb::GrantTemplate, CliError> {
    let client = pb::GrantServiceClient::new(c.http(), c.config());
    let request = pb::GetGrantTemplateRequest {
        id: id.to_owned(),
        version,
        at: place.into(),
        ..Default::default()
    };
    let got = client.get_grant_template(request).await?.into_owned().template;
    got.into_option().ok_or_else(|| local("the template came back empty".into()))
}

/// A template as written in a file.
fn from_file(file: &Path) -> Result<pb::GrantTemplate, CliError> {
    let at = file.display();
    let text = std::fs::read_to_string(file).map_err(|e| local(format!("{at}: {e}")))?;
    serde_saphyr::from_str(&text).map_err(|e| local(format!("{at}: {e}")))
}

/// What a template file asks for.
fn body(t: &pb::GrantTemplate) -> pb::GrantTemplateBody {
    pb::GrantTemplateBody {
        name: t.name.clone(),
        actions: t.actions.clone(),
        scope_pattern: t.scope_pattern.clone(),
        delegation: t.delegation.clone(),
        conditions: t.conditions.clone(),
        ..Default::default()
    }
}

/// Seconds as a person reads a lifetime.
fn span(seconds: i64) -> String {
    match seconds {
        s if s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// Where a template applies, as a person reads it.
fn applied(t: &pb::GrantTemplate) -> String {
    let pattern = t.scope_pattern.as_option();
    let kind = pattern.map(|p| short(p.kind, "OBJECT_KIND_")).filter(|k| k != "unspecified");
    let kind = kind.unwrap_or_else(|| "organization, project, or workload".into());
    let labels = pattern.and_then(|p| p.selector.as_option()).map(|s| {
        let pairs: Vec<String> = s.match_labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!(" [{}]", pairs.join(","))
    });
    format!("{kind}{}", labels.unwrap_or_default())
}

/// A template in full, as a person reads it.
fn shown(t: &pb::GrantTemplate) -> String {
    let mut out =
        format!("{} ({}) version {}, owned by {}\n", t.name, t.id, t.version, t.organization);
    let _ = writeln!(out, "  applied to: {}", applied(t));
    let _ = writeln!(out, "  actions:    {}", t.actions.join(", "));
    let d = t.delegation.as_option();
    let delegates = match d.filter(|d| d.allowed) {
        Some(d) => format!("{}, {} level(s) deep", d.actions.join(", "), d.max_depth.unwrap_or(0)),
        None => "nothing".into(),
    };
    let _ = writeln!(out, "  delegates:  {delegates}");
    if let Some(c) = t.conditions.as_option() {
        let mut said = Vec::new();
        if let Some(l) = c.lifetime.as_option() {
            said.push(format!("expires {} after issue", span(l.seconds)));
        }
        if c.requires_mfa {
            said.push("a second factor at every use".into());
        }
        if let Some(n) = c.max_uses {
            said.push(format!("{n} uses"));
        }
        if !c.source_cidr.is_empty() {
            said.push(format!("from {}", c.source_cidr.join(", ")));
        }
        if !said.is_empty() {
            let _ = writeln!(out, "  defaults:   {}", said.join("; "));
        }
    }
    if let Some(r) = t.retired_at.as_option() {
        let _ = writeln!(out, "  retired {}", time(Some(r)));
    }
    out
}

fn rows(list: &[pb::GrantTemplate]) -> String {
    let header = ["ID", "NAME", "VERSION", "OWNER", "APPLIED TO", "ACTIONS", "STATE"];
    let mut all = vec![header.map(str::to_owned).to_vec()];
    all.extend(list.iter().map(|t| {
        let state = if t.retired_at.as_option().is_some() { "retired" } else { "live" };
        vec![
            t.id.clone(),
            t.name.clone(),
            t.version.to_string(),
            t.organization.clone(),
            applied(t),
            t.actions.join(","),
            state.into(),
        ]
    }));
    table(&all)
}

/// Answers `korpis template holders`.
async fn holders(
    c: &Connection,
    id: &str,
    place: Option<pb::ObjectRef>,
) -> Result<Answer, CliError> {
    // The current version marks those behind it, when the caller may read the template.
    let current = read(c, id, None, place).await.ok().map(|t| t.version);
    let client = pb::GrantServiceClient::new(c.http(), c.config());
    let mut grants = Vec::new();
    let mut page_token = String::new();
    loop {
        let request = pb::ListGrantsRequest {
            filter: Some(list_grants_request::Filter::Template(id.to_owned())),
            page: pb::PageRequest { page_token, ..Default::default() }.into(),
            ..Default::default()
        };
        let listed = client.list_grants(request).await?.into_owned();
        grants.extend(listed.grants);
        page_token = listed.page.as_option().map(|p| p.next_page_token.clone()).unwrap_or_default();
        if page_token.is_empty() {
            break;
        }
    }
    let mut all =
        vec![["GRANT", "HOLDER", "SCOPE", "STATE", "VERSION"].map(str::to_owned).to_vec()];
    all.extend(grants.iter().map(|g| {
        let version = g.from_template.as_option().map_or(0, |t| t.version);
        let behind = current.is_some_and(|c| version < c);
        vec![
            g.id.clone(),
            who(g.subject.as_option()),
            grant::place(g.scope.as_option()),
            short(g.state, "GRANT_STATE_"),
            if behind { format!("{version} (behind)") } else { version.to_string() },
        ]
    }));
    Ok(Answer { human: table(&all), json: json(&grants) })
}

/// Answers `korpis template edit`: the file's template, made the next version of the one it
/// was read at.
async fn edit(
    c: &Connection,
    template: &str,
    file: &Path,
    org: Option<&str>,
) -> Result<Answer, CliError> {
    let (id, _) = named(c, template, org_at(c, org).await?).await?;
    let edited = from_file(file)?;
    if edited.version == 0 {
        let why = "the file says the version it was read at, as `template show --json` does";
        return Err(local(format!("{}: {why}", file.display())));
    }
    let request = pb::UpdateGrantTemplateRequest {
        mutation: mutation(Some(edited.version)).into(),
        id,
        template: body(&edited).into(),
        ..Default::default()
    };
    let client = pb::GrantServiceClient::new(c.http(), c.config());
    let made = client.update_grant_template(request).await?.into_owned();
    let t = made.template.as_option();
    Ok(Answer { human: t.map(shown).unwrap_or_default(), json: json(&t) })
}

/// Answers `korpis template …`.
pub async fn template(c: &Connection, a: &TemplateAction) -> Result<Answer, CliError> {
    let client = pb::GrantServiceClient::new(c.http(), c.config());
    let one = |t: Option<&pb::GrantTemplate>| Answer {
        human: t.map(shown).unwrap_or_default(),
        json: json(&t),
    };
    match a {
        TemplateAction::Create { organization, file } => {
            let request = pb::CreateGrantTemplateRequest {
                mutation: mutation(None).into(),
                organization: resolve::organization(c, Some(organization)).await?,
                template: body(&from_file(file)?).into(),
                ..Default::default()
            };
            let made = client.create_grant_template(request).await?.into_owned();
            Ok(one(made.template.as_option()))
        }
        TemplateAction::List { organization, owned } => {
            let org = resolve::organization(c, organization.as_deref()).await?;
            let of = if *owned {
                list_grant_templates_request::Of::Owned(org)
            } else {
                list_grant_templates_request::Of::At(Box::new(at(&org)))
            };
            let list = templates(c, of).await?;
            Ok(Answer { human: rows(&list), json: json(&list) })
        }
        TemplateAction::Show { template, version, org } => {
            let (id, place) = named(c, template, org_at(c, org.as_deref()).await?).await?;
            Ok(one(Some(&read(c, &id, *version, place).await?)))
        }
        TemplateAction::Edit { template, file, org } => {
            edit(c, template, file, org.as_deref()).await
        }
        TemplateAction::Retire { template, org } => {
            let (id, _) = named(c, template, org_at(c, org.as_deref()).await?).await?;
            let request = pb::RetireGrantTemplateRequest {
                mutation: mutation(None).into(),
                id,
                ..Default::default()
            };
            let made = client.retire_grant_template(request).await?.into_owned();
            Ok(one(made.template.as_option()))
        }
        TemplateAction::Holders { template, org } => {
            let (id, place) = named(c, template, org_at(c, org.as_deref()).await?).await?;
            holders(c, &id, place).await
        }
    }
}
