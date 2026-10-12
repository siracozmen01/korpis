// SPDX-License-Identifier: AGPL-3.0-or-later

//! The registry client: resolving a recipe's version to its digest, fetching the artifact by
//! digest, and pushing one. Every byte fetched is checked against the digest that named it here,
//! whatever the client library already checked: the digest is the recipe's identity.

use crate::artifact::{
    ARTIFACT_TYPE, Artifact, DOCUMENT_MEDIA_TYPE, EMPTY_CONFIG_MEDIA_TYPE, File, LayoutError,
    TITLE, check_files,
};
use crate::document::MAX_DOCUMENT;
use oci_client::client::{ClientConfig, ClientProtocol, Config, ImageLayer};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;

mod signature;

/// Why a registry operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// No such recipe or version.
    #[error("not found: {0}")]
    NotFound(String),
    /// The registry could not be reached or refused; retrying may help.
    #[error("registry unavailable: {0}")]
    Unavailable(String),
    /// What came back is not a well-formed recipe artifact, or not the one asked for.
    #[error("{0}")]
    Invalid(String),
}

impl From<LayoutError> for RegistryError {
    fn from(e: LayoutError) -> Self {
        Self::Invalid(e.to_string())
    }
}

fn classify(e: &oci_client::errors::OciDistributionError) -> RegistryError {
    use oci_client::errors::OciDistributionError as E;
    match e {
        E::ImageManifestNotFoundError(m) => RegistryError::NotFound(m.clone()),
        E::RegistryError { envelope, .. }
            if envelope.errors.iter().any(|x| format!("{:?}", x.code).contains("Unknown")) =>
        {
            RegistryError::NotFound(e.to_string())
        }
        E::DigestError(_) => RegistryError::Invalid(e.to_string()),
        _ => RegistryError::Unavailable(e.to_string()),
    }
}

/// The sha256 digest of `bytes`.
#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::from("sha256:"), |mut s, b| {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A pulled artifact, verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pulled {
    /// The recipe document.
    pub document: Vec<u8>,
    /// The paths of the files the artifact carries.
    pub files: Vec<String>,
}

/// Access to OCI registries.
pub struct Registry {
    client: Client,
    auth: RegistryAuth,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").finish_non_exhaustive()
    }
}

fn reference(s: &str) -> Result<Reference, RegistryError> {
    s.parse().map_err(|e| RegistryError::Invalid(format!("{s:?}: {e}")))
}

impl Registry {
    /// A client speaking HTTPS to every registry except those named in `plain_http`, which are
    /// development registries on loopback.
    #[must_use]
    pub fn new(plain_http: Vec<String>) -> Self {
        // One TLS provider for the whole process: ring, as everywhere else in Korpis. Installing
        // it a second time fails harmlessly, so whoever installs it first wins and it is ring.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let protocol = if plain_http.is_empty() {
            ClientProtocol::Https
        } else {
            ClientProtocol::HttpsExcept(plain_http)
        };
        let client = Client::new(ClientConfig { protocol, ..ClientConfig::default() });
        Self { client, auth: RegistryAuth::Anonymous }
    }

    /// The digest `name:version` points to now.
    pub async fn resolve(&self, name: &str, version: &str) -> Result<String, RegistryError> {
        let r = reference(&format!("{name}:{version}"))?;
        let digest =
            self.client.fetch_manifest_digest(&r, &self.auth).await.map_err(|e| classify(&e))?;
        if korpis_core::recipe::reference::is_digest(&digest) {
            Ok(digest)
        } else {
            Err(RegistryError::Invalid(format!("the registry answered with digest {digest:?}")))
        }
    }

    async fn blob(
        &self,
        r: &Reference,
        layer: &OciDescriptor,
        max: usize,
    ) -> Result<Vec<u8>, RegistryError> {
        if usize::try_from(layer.size).map_or(true, |s| s > max) {
            return Err(RegistryError::Invalid(format!(
                "layer {} is larger than {max} bytes",
                layer.digest
            )));
        }
        let mut out = Vec::new();
        self.client.pull_blob(r, layer, &mut out).await.map_err(|e| classify(&e))?;
        if sha256(&out) != layer.digest {
            return Err(RegistryError::Invalid(format!(
                "layer content does not match {}",
                layer.digest
            )));
        }
        Ok(out)
    }

    /// The artifact `name@digest`, checked byte for byte against the digest.
    pub async fn pull(&self, name: &str, digest: &str) -> Result<Pulled, RegistryError> {
        let r = reference(&format!("{name}@{digest}"))?;
        let (raw, _) = self
            .client
            .pull_manifest_raw(&r, &self.auth, &[OCI_IMAGE_MEDIA_TYPE])
            .await
            .map_err(|e| classify(&e))?;
        if sha256(&raw) != digest {
            return Err(RegistryError::Invalid(format!("the manifest does not match {digest}")));
        }
        let manifest: OciImageManifest =
            serde_json::from_slice(&raw).map_err(|e| RegistryError::Invalid(e.to_string()))?;
        if manifest.artifact_type.as_deref() != Some(ARTIFACT_TYPE) {
            return Err(RegistryError::Invalid(format!("{name}@{digest} is not a Korpis recipe")));
        }
        let mut documents = manifest.layers.iter().filter(|l| l.media_type == DOCUMENT_MEDIA_TYPE);
        let (Some(doc), None) = (documents.next(), documents.next()) else {
            return Err(RegistryError::Invalid(
                "a recipe artifact has exactly one document layer".into(),
            ));
        };
        let document = self.blob(&r, doc, MAX_DOCUMENT).await?;
        let files = manifest
            .layers
            .iter()
            .filter(|l| l.media_type != DOCUMENT_MEDIA_TYPE)
            .map(|l| {
                l.annotations.as_ref().and_then(|a| a.get(TITLE)).cloned().ok_or_else(|| {
                    RegistryError::Invalid(format!("layer {} has no {TITLE}", l.digest))
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Pulled { document, files })
    }

    /// Pushes `artifact` as `name:version`; returns the manifest's digest.
    pub async fn push(
        &self,
        name: &str,
        version: &str,
        artifact: &Artifact,
    ) -> Result<String, RegistryError> {
        let r = reference(&format!("{name}:{version}"))?;
        // What could not be pulled is not pushed.
        let recipe = crate::document::parse(&artifact.document)
            .map_err(|e| RegistryError::Invalid(e.to_string()))?;
        let titles: Vec<String> = artifact.files.iter().map(|f| f.path.clone()).collect();
        check_files(&recipe, &titles).map_err(|e| RegistryError::Invalid(e.to_string()))?;
        let mut layers =
            vec![ImageLayer::new(artifact.document.clone(), DOCUMENT_MEDIA_TYPE.to_owned(), None)];
        for File { path, media_type, data } in &artifact.files {
            let title = BTreeMap::from([(TITLE.to_owned(), path.clone())]);
            layers.push(ImageLayer::new(data.clone(), media_type.clone(), Some(title)));
        }
        let config = Config::new(b"{}".to_vec(), EMPTY_CONFIG_MEDIA_TYPE.to_owned(), None);
        let mut manifest = OciImageManifest::build(&layers, &config, None);
        manifest.artifact_type = Some(ARTIFACT_TYPE.to_owned());
        // The response carries only URLs; the digest is read back so it is the registry's own.
        self.client
            .push(&r, &layers, config, &self.auth, Some(manifest))
            .await
            .map_err(|e| classify(&e))?;
        self.resolve(name, version).await
    }
}
