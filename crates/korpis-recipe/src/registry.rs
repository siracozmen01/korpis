// SPDX-License-Identifier: AGPL-3.0-or-later

//! The registry client: resolving a recipe's version to its digest, fetching the artifact by
//! digest, and pushing one. Every byte fetched is checked against the digest that named it here,
//! whatever the client library already checked: the digest is the recipe's identity.

use crate::artifact::{
    Artifact, EMPTY_CONFIG_MEDIA_TYPE, File, Kind, LayoutError, TITLE, check_files,
};
use crate::document::MAX_DOCUMENT;
use korpis_egress::{Proxy, Reach};
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

/// What `e` says, and what caused it: a refusal by the proxy is otherwise reported as a request
/// that could not be sent, which says nothing of why.
fn described(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        let _ = write!(out, ": {c}");
        cause = c.source();
    }
    out
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
        _ => RegistryError::Unavailable(described(e)),
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
    /// Which kind its manifest says it is.
    pub kind: Kind,
    /// The document.
    pub document: Vec<u8>,
    /// The paths of the files the artifact carries.
    pub files: Vec<String>,
}

/// Access to OCI registries.
pub struct Registry {
    client: Client,
    auth: RegistryAuth,
    /// What the client connects through, when what it reaches is bounded.
    _proxy: Option<Proxy>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").finish_non_exhaustive()
    }
}

/// How a client speaks to registries: HTTPS to every one but those in `plain_http`.
fn config(plain_http: Vec<String>) -> ClientConfig {
    // One TLS provider for the whole process: ring, as everywhere else in Korpis. Installing it a
    // second time fails harmlessly, so whoever installs it first wins and it is ring.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let protocol = if plain_http.is_empty() {
        ClientProtocol::Https
    } else {
        ClientProtocol::HttpsExcept(plain_http)
    };
    ClientConfig {
        protocol,
        // A registry that stalls fails the call rather than holding its caller forever.
        connect_timeout: Some(std::time::Duration::from_secs(30)),
        read_timeout: Some(std::time::Duration::from_secs(120)),
        ..ClientConfig::default()
    }
}

fn reference(s: &str) -> Result<Reference, RegistryError> {
    s.parse().map_err(|e| RegistryError::Invalid(format!("{s:?}: {e}")))
}

impl Registry {
    /// A client for the control plane, speaking HTTPS to every registry except those named in
    /// `plain_http`, which are development registries, and reaching only the public internet and
    /// those: a recipe's registry is named by whoever declares it (§3 of docs/17-security.md).
    ///
    /// # Errors
    /// The proxy it connects through could not be started.
    pub fn new(plain_http: Vec<String>) -> Result<Self, RegistryError> {
        Self::reaching(plain_http, Reach::default())
    }

    /// [`Self::new`], also reaching the networks `reach` allows.
    ///
    /// # Errors
    /// The proxy it connects through could not be started.
    pub fn reaching(plain_http: Vec<String>, reach: Reach) -> Result<Self, RegistryError> {
        let unavailable = |e: std::io::Error| RegistryError::Unavailable(e.to_string());
        let proxy = Proxy::start(reach, plain_http.clone()).map_err(unavailable)?;
        let config = ClientConfig {
            https_proxy: Some(proxy.url().to_owned()),
            http_proxy: Some(proxy.url().to_owned()),
            ..config(plain_http)
        };
        // `Client::new` would build a client without the proxy if this failed; this one refuses.
        let client =
            Client::try_from(config).map_err(|e| RegistryError::Unavailable(e.to_string()))?;
        Ok(Self { client, auth: RegistryAuth::Anonymous, _proxy: Some(proxy) })
    }

    /// A client for a person's own machine, the CLI's, reaching whatever its user can: the
    /// registries it names are its user's to name.
    #[must_use]
    pub fn client_side(plain_http: Vec<String>) -> Self {
        Self {
            client: Client::new(config(plain_http)),
            auth: RegistryAuth::Anonymous,
            _proxy: None,
        }
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
        let Some(kind) = manifest.artifact_type.as_deref().and_then(Kind::of_artifact_type) else {
            let why = format!("{name}@{digest} is not a Korpis recipe or extension");
            return Err(RegistryError::Invalid(why));
        };
        let document_type = kind.document_media_type();
        let mut documents = manifest.layers.iter().filter(|l| l.media_type == document_type);
        let (Some(doc), None) = (documents.next(), documents.next()) else {
            return Err(RegistryError::Invalid(format!(
                "a {} artifact has exactly one document layer",
                kind.artifact_type()
            )));
        };
        let document = self.blob(&r, doc, MAX_DOCUMENT).await?;
        let files = manifest
            .layers
            .iter()
            .filter(|l| l.media_type != document_type)
            .map(|l| {
                l.annotations.as_ref().and_then(|a| a.get(TITLE)).cloned().ok_or_else(|| {
                    RegistryError::Invalid(format!("layer {} has no {TITLE}", l.digest))
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Pulled { kind, document, files })
    }

    /// Pushes `artifact` as `name:version`, as the kind its document is; returns the manifest's
    /// digest.
    pub async fn push(
        &self,
        name: &str,
        version: &str,
        artifact: &Artifact,
    ) -> Result<String, RegistryError> {
        let r = reference(&format!("{name}:{version}"))?;
        // What could not be pulled is not pushed.
        let parsed = crate::document::parse_document(&artifact.document)
            .map_err(|e| RegistryError::Invalid(e.to_string()))?;
        let kind = parsed.kind();
        let titles: Vec<String> = artifact.files.iter().map(|f| f.path.clone()).collect();
        check_files(&parsed.recipe, &titles).map_err(|e| RegistryError::Invalid(e.to_string()))?;
        let document_type = kind.document_media_type().to_owned();
        let mut layers = vec![ImageLayer::new(artifact.document.clone(), document_type, None)];
        for File { path, media_type, data } in &artifact.files {
            let title = BTreeMap::from([(TITLE.to_owned(), path.clone())]);
            layers.push(ImageLayer::new(data.clone(), media_type.clone(), Some(title)));
        }
        let config = Config::new(b"{}".to_vec(), EMPTY_CONFIG_MEDIA_TYPE.to_owned(), None);
        let mut manifest = OciImageManifest::build(&layers, &config, None);
        manifest.artifact_type = Some(kind.artifact_type().to_owned());
        // The response carries only URLs; the digest is read back so it is the registry's own.
        self.client
            .push(&r, &layers, config, &self.auth, Some(manifest))
            .await
            .map_err(|e| classify(&e))?;
        self.resolve(name, version).await
    }
}
