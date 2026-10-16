// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis certificate …` (§3.2 of docs/07-networking.md): the certificates an organization
//! uploads for its endpoints to serve with, named in an endpoint's `tls.secret`.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;
use std::path::{Path, PathBuf};

/// `korpis certificate …`.
#[derive(Debug, Clone, Subcommand)]
pub enum CertificateAction {
    /// Uploads a certificate: its chain, leaf first, and the leaf's key, both PEM files.
    Create {
        /// The chain.
        #[arg(long)]
        chain: PathBuf,
        /// The leaf's private key.
        #[arg(long)]
        key: PathBuf,
        /// The organization that will hold it, by name or identifier; the top one you can read
        /// by default.
        #[arg(long)]
        organization: Option<String>,
    },
    /// Replaces a certificate's chain and key, keeping its identifier, so the endpoints serving
    /// with it serve the new one.
    Replace {
        /// The certificate, `crt_…`.
        certificate: String,
        /// The new chain.
        #[arg(long)]
        chain: PathBuf,
        /// The new chain's leaf's private key.
        #[arg(long)]
        key: PathBuf,
    },
    /// The certificates of an organization.
    List {
        /// The organization, by name or identifier.
        #[arg(long)]
        organization: Option<String>,
    },
    /// One certificate, with its chain.
    Get {
        /// The certificate, `crt_…`.
        certificate: String,
    },
    /// Deletes a certificate no endpoint serves with.
    Delete {
        /// The certificate, `crt_…`.
        certificate: String,
    },
}

fn read(path: &Path) -> Result<String, CliError> {
    std::fs::read_to_string(path).map_err(|e| CliError::Local(format!("{}: {e}", path.display())))
}

fn rows(list: &[pb::Certificate]) -> String {
    let mut all =
        vec![["ID", "NAMES", "SOURCE", "STATE", "EXPIRES", "REVISION"].map(str::to_owned).to_vec()];
    all.extend(list.iter().map(|c| {
        vec![
            c.id.clone(),
            c.names.join(","),
            short(c.source, "CERTIFICATE_SOURCE_").to_lowercase(),
            short(c.state, "CERTIFICATE_STATE_").to_lowercase(),
            time(c.not_after.as_option()),
            c.revision.to_string(),
        ]
    }));
    table(&all)
}

fn shown(c: &pb::Certificate) -> Answer {
    let reason = if c.reason.is_empty() { String::new() } else { format!("\n{}\n", c.reason) };
    Answer { human: format!("{}{reason}", rows(std::slice::from_ref(c))), json: json(c) }
}

/// Answers `korpis certificate …`.
pub async fn certificate(c: &Connection, action: &CertificateAction) -> Result<Answer, CliError> {
    let client = pb::CertificateServiceClient::new(c.http(), c.config());
    Ok(match action {
        CertificateAction::Create { chain, key, organization } => {
            let r = pb::CreateCertificateRequest {
                mutation: mutation(None).into(),
                organization: resolve::organization(c, organization.as_deref()).await?,
                chain: read(chain)?,
                key: read(key)?,
                ..Default::default()
            };
            let made = client.create_certificate(r).await?.into_owned().certificate;
            shown(&made.into_option().unwrap_or_default())
        }
        CertificateAction::Replace { certificate, chain, key } => {
            let r = pb::ReplaceCertificateRequest {
                mutation: mutation(None).into(),
                id: certificate.clone(),
                chain: read(chain)?,
                key: read(key)?,
                ..Default::default()
            };
            let made = client.replace_certificate(r).await?.into_owned().certificate;
            shown(&made.into_option().unwrap_or_default())
        }
        CertificateAction::List { organization } => {
            let r = pb::ListCertificatesRequest {
                organization: resolve::organization(c, organization.as_deref()).await?,
                ..Default::default()
            };
            let list = client.list_certificates(r).await?.into_owned().certificates;
            Answer { human: rows(&list), json: json(&list) }
        }
        CertificateAction::Get { certificate } => {
            let r = pb::GetCertificateRequest { id: certificate.clone(), ..Default::default() };
            let got = client.get_certificate(r).await?.into_owned().certificate;
            let got = got.into_option().unwrap_or_default();
            let mut answer = shown(&got);
            answer.human.push('\n');
            answer.human.push_str(&got.chain);
            answer
        }
        CertificateAction::Delete { certificate } => {
            let r = pb::DeleteCertificateRequest {
                mutation: mutation(None).into(),
                id: certificate.clone(),
                ..Default::default()
            };
            client.delete_certificate(r).await?;
            Answer { human: "deleted\n".into(), json: serde_json::json!({ "deleted": true }) }
        }
    })
}
