// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis domain …` (§3.2 of docs/07-networking.md): the names an organization holds, which its
//! endpoints may route once verified.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;

/// `korpis domain …`.
#[derive(Debug, Clone, Subcommand)]
pub enum DomainAction {
    /// Asks for a domain. By default it is verified by a TXT record you publish and then
    /// `korpis domain verify`; delegated beneath an ancestor's domain, or attested by an operator,
    /// it is verified at once.
    Create {
        /// The name, e.g. `shop.example.com`; a name in another script in its `xn--` form.
        name: String,
        /// The organization that will hold it, by name or identifier; the top one you can read
        /// by default.
        #[arg(long)]
        organization: Option<String>,
        /// Delegate it beneath this verified domain of an ancestor organization, `dom_…`.
        #[arg(long, conflicts_with = "attest")]
        delegated_from: Option<String>,
        /// Attest it as an operator of the installation.
        #[arg(long)]
        attest: bool,
    },
    /// Verifies a domain by its TXT record.
    Verify {
        /// The domain, `dom_…`.
        domain: String,
    },
    /// The domains of an organization.
    List {
        /// The organization, by name or identifier.
        #[arg(long)]
        organization: Option<String>,
    },
    /// One domain, with the record that verifies it.
    Get {
        /// The domain, `dom_…`.
        domain: String,
    },
    /// Deletes a domain, and every domain delegated beneath it.
    Delete {
        /// The domain, `dom_…`.
        domain: String,
    },
}

fn rows(list: &[pb::Domain]) -> String {
    let mut all =
        vec![["ID", "NAME", "VERIFICATION", "VERIFIED", "MADE"].map(str::to_owned).to_vec()];
    all.extend(list.iter().map(|d| {
        let verified =
            d.verified_at.as_option().map_or_else(|| "pending".into(), |t| time(Some(t)));
        vec![
            d.id.clone(),
            d.name.clone(),
            short(d.verification, "DOMAIN_VERIFICATION_").to_lowercase(),
            verified,
            time(d.created_at.as_option()),
        ]
    }));
    table(&all)
}

/// The table, and for a pending domain what to publish.
fn shown(d: &pb::Domain) -> Answer {
    let table = rows(std::slice::from_ref(d));
    let human = if d.verified_at.as_option().is_none() && !d.record.is_empty() {
        let (id, record, token) = (&d.id, &d.record, &d.token);
        format!(
            "{table}\nPublish this TXT record, then run `korpis domain verify {id}`:\n\n  {record}  TXT  \"{token}\"\n"
        )
    } else {
        table
    };
    Answer { human, json: json(d) }
}

async fn create(c: &Connection, action: &DomainAction) -> Result<pb::Domain, CliError> {
    let DomainAction::Create { name, organization, delegated_from, attest } = action else {
        return Err(CliError::Local("not a create".into()));
    };
    let verification = match (delegated_from, attest) {
        (Some(_), _) => pb::DomainVerification::DOMAIN_VERIFICATION_DELEGATED,
        (None, true) => pb::DomainVerification::DOMAIN_VERIFICATION_OPERATOR,
        (None, false) => pb::DomainVerification::DOMAIN_VERIFICATION_DNS_TXT,
    };
    let r = pb::CreateDomainRequest {
        mutation: mutation(None).into(),
        organization: resolve::organization(c, organization.as_deref()).await?,
        name: name.clone(),
        verification: verification.into(),
        parent: delegated_from.clone(),
        ..Default::default()
    };
    let client = pb::DomainServiceClient::new(c.http(), c.config());
    Ok(client.create_domain(r).await?.into_owned().domain.into_option().unwrap_or_default())
}

/// Answers `korpis domain …`.
pub async fn domain(c: &Connection, action: &DomainAction) -> Result<Answer, CliError> {
    let client = pb::DomainServiceClient::new(c.http(), c.config());
    Ok(match action {
        DomainAction::Create { .. } => shown(&create(c, action).await?),
        DomainAction::Verify { domain } => {
            let r = pb::VerifyDomainRequest {
                mutation: mutation(None).into(),
                id: domain.clone(),
                ..Default::default()
            };
            shown(
                &client
                    .verify_domain(r)
                    .await?
                    .into_owned()
                    .domain
                    .into_option()
                    .unwrap_or_default(),
            )
        }
        DomainAction::List { organization } => {
            let r = pb::ListDomainsRequest {
                organization: resolve::organization(c, organization.as_deref()).await?,
                ..Default::default()
            };
            let list = client.list_domains(r).await?.into_owned().domains;
            Answer { human: rows(&list), json: json(&list) }
        }
        DomainAction::Get { domain } => {
            let r = pb::GetDomainRequest { id: domain.clone(), ..Default::default() };
            shown(
                &client.get_domain(r).await?.into_owned().domain.into_option().unwrap_or_default(),
            )
        }
        DomainAction::Delete { domain } => {
            let r = pb::DeleteDomainRequest {
                mutation: mutation(None).into(),
                id: domain.clone(),
                ..Default::default()
            };
            let deleted = client.delete_domain(r).await?.into_owned().deleted;
            let human =
                format!("deleted {deleted} domain{}\n", if deleted == 1 { "" } else { "s" });
            Answer { human, json: serde_json::json!({ "deleted": deleted }) }
        }
    })
}
