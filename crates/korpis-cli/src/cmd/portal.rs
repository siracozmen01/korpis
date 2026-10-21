// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis portal …` (§3.6 of docs/07-networking.md): the names the control plane answers under
//! for an organization, with its theme and its sign-in.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;

/// `korpis portal …`.
#[derive(Debug, Clone, Subcommand)]
pub enum PortalAction {
    /// Makes a portal of a name the organization holds, served with a certificate of its own
    /// that the control plane orders, unless `--certificate` names a provided one.
    Create {
        /// The name, e.g. `panel.reseller.example`.
        name: String,
        /// The organization whose portal it is, by name or identifier; the top one you can read
        /// by default.
        #[arg(long)]
        organization: Option<String>,
        /// A provided certificate of the organization covering the name, `crt_…`.
        #[arg(long)]
        certificate: Option<String>,
    },
    /// Serves a portal with another certificate, keeping everything else.
    Certificate {
        /// The portal, `ptl_…`.
        portal: String,
        /// A provided certificate of its organization covering its name, `crt_…`.
        #[arg(required_unless_present = "acme", conflicts_with = "acme")]
        certificate: Option<String>,
        /// Its own certificate, ordered by the control plane, again.
        #[arg(long)]
        acme: bool,
    },
    /// The portals of an organization.
    List {
        /// The organization, by name or identifier.
        #[arg(long)]
        organization: Option<String>,
    },
    /// One portal, with its certificate's state.
    Get {
        /// The portal, `ptl_…`.
        portal: String,
    },
    /// Deletes a portal.
    Delete {
        /// The portal, `ptl_…`.
        portal: String,
        /// Deleting a portal ends every session begun at it and removes every passkey registered
        /// there; say so.
        #[arg(long)]
        yes: bool,
    },
}

fn rows(list: &[pb::Portal]) -> String {
    let mut all =
        vec![["ID", "NAME", "CERTIFICATE", "SOURCE", "STATE", "MADE"].map(str::to_owned).to_vec()];
    all.extend(list.iter().map(|p| {
        vec![
            p.id.clone(),
            p.name.clone(),
            p.certificate.clone(),
            short(p.certificate_source, "CERTIFICATE_SOURCE_").to_lowercase(),
            short(p.certificate_state, "CERTIFICATE_STATE_").to_lowercase(),
            time(p.created_at.as_option()),
        ]
    }));
    table(&all)
}

/// The table, and why its certificate is not issued where it is not.
fn shown(p: &pb::Portal) -> Answer {
    let table = rows(std::slice::from_ref(p));
    let human = if p.certificate_reason.is_empty() {
        table
    } else {
        format!("{table}\nIts certificate: {}\n", p.certificate_reason)
    };
    Answer { human, json: json(p) }
}

fn portal_of(p: Option<pb::Portal>) -> pb::Portal {
    p.unwrap_or_default()
}

async fn create(c: &Connection, action: &PortalAction) -> Result<pb::Portal, CliError> {
    let PortalAction::Create { name, organization, certificate } = action else {
        return Err(CliError::Local("not a create".into()));
    };
    let r = pb::CreatePortalRequest {
        mutation: mutation(None).into(),
        organization: resolve::organization(c, organization.as_deref()).await?,
        name: name.clone(),
        certificate: certificate.clone().unwrap_or_default(),
        ..Default::default()
    };
    let client = pb::PortalServiceClient::new(c.http(), c.config());
    Ok(portal_of(client.create_portal(r).await?.into_owned().portal.into_option()))
}

/// Answers `korpis portal …`.
pub async fn portal(c: &Connection, action: &PortalAction) -> Result<Answer, CliError> {
    let client = pb::PortalServiceClient::new(c.http(), c.config());
    Ok(match action {
        PortalAction::Create { .. } => shown(&create(c, action).await?),
        PortalAction::Certificate { portal, certificate, .. } => {
            let r = pb::SetPortalCertificateRequest {
                mutation: mutation(None).into(),
                id: portal.clone(),
                certificate: certificate.clone().unwrap_or_default(),
                ..Default::default()
            };
            shown(&portal_of(
                client.set_portal_certificate(r).await?.into_owned().portal.into_option(),
            ))
        }
        PortalAction::List { organization } => {
            let r = pb::ListPortalsRequest {
                organization: resolve::organization(c, organization.as_deref()).await?,
                ..Default::default()
            };
            let list = client.list_portals(r).await?.into_owned().portals;
            Answer { human: rows(&list), json: json(&list) }
        }
        PortalAction::Get { portal } => {
            let r = pb::GetPortalRequest { id: portal.clone(), ..Default::default() };
            shown(&portal_of(client.get_portal(r).await?.into_owned().portal.into_option()))
        }
        PortalAction::Delete { portal, yes } => {
            if !yes {
                let why = "deleting a portal ends every session begun at it and removes every \
                           passkey registered there; pass --yes to delete it";
                return Err(CliError::Local(why.into()));
            }
            let r = pb::DeletePortalRequest {
                mutation: mutation(None).into(),
                id: portal.clone(),
                ..Default::default()
            };
            client.delete_portal(r).await?;
            Answer { human: format!("deleted {portal}\n"), json: serde_json::json!({}) }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_portal_is_shown_with_its_certificate_s_state_and_why_it_is_not_issued() {
        let p = pb::Portal {
            id: "ptl_1".into(),
            name: "panel.reseller.example".into(),
            certificate: "crt_1".into(),
            certificate_source: pb::CertificateSource::CERTIFICATE_SOURCE_ACME.into(),
            certificate_state: pb::CertificateState::CERTIFICATE_STATE_FAILED.into(),
            certificate_reason: "no DNS provider serves its zone".into(),
            ..Default::default()
        };
        let human = shown(&p).human;
        assert!(human.contains("panel.reseller.example"), "{human}");
        assert!(human.contains("acme") && human.contains("failed"), "{human}");
        assert!(human.ends_with("Its certificate: no DNS provider serves its zone\n"), "{human}");
    }
}
