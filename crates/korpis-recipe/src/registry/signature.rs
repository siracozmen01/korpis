// SPDX-License-Identifier: AGPL-3.0-or-later

//! cosign signatures beside a recipe: read before anything of the recipe is fetched (§7 of
//! docs/09-recipes.md), and written the way `cosign sign --key` writes them, for the signing
//! tooling of Phase 4 and for tests.

use super::{Registry, RegistryError, classify, reference};
use crate::artifact::EMPTY_CONFIG_MEDIA_TYPE;
use korpis_core::recipe::trust::Signature;
use korpis_core::recipe::trust::cosign::{
    SIGNATURE_ANNOTATION, SIMPLE_SIGNING_MEDIA_TYPE, payload, sign, signature_tag,
};
use oci_client::client::{Config, ImageLayer};
use oci_client::manifest::{IMAGE_MANIFEST_MEDIA_TYPE, OCI_IMAGE_MEDIA_TYPE, OciImageManifest};
use std::collections::BTreeMap;

/// The largest signature payload read: a simple-signing payload is a few hundred bytes.
const MAX_PAYLOAD: usize = 64 * 1024;

fn signature_ref(name: &str, digest: &str) -> Result<oci_client::Reference, RegistryError> {
    let tag = signature_tag(digest)
        .ok_or_else(|| RegistryError::Invalid(format!("{digest:?} is not a digest")))?;
    reference(&format!("{name}:{tag}"))
}

impl Registry {
    /// Every cosign signature stored beside `name@digest`; none if it was never signed. Reading
    /// them fetches nothing of the recipe itself.
    pub async fn signatures(
        &self,
        name: &str,
        digest: &str,
    ) -> Result<Vec<Signature>, RegistryError> {
        let r = signature_ref(name, digest)?;
        let accepted = [OCI_IMAGE_MEDIA_TYPE, IMAGE_MANIFEST_MEDIA_TYPE];
        let raw = match self.client.pull_manifest_raw(&r, &self.auth, &accepted).await {
            Ok((raw, _)) => raw,
            Err(e) => {
                return match classify(&e) {
                    RegistryError::NotFound(_) => Ok(Vec::new()),
                    other => Err(other),
                };
            }
        };
        let manifest: OciImageManifest =
            serde_json::from_slice(&raw).map_err(|e| RegistryError::Invalid(e.to_string()))?;
        let mut out = Vec::new();
        for layer in manifest.layers.iter().filter(|l| l.media_type == SIMPLE_SIGNING_MEDIA_TYPE) {
            let annotation = layer.annotations.as_ref().and_then(|a| a.get(SIGNATURE_ANNOTATION));
            let Some(signature) = annotation else { continue };
            let payload = self.blob(&r, layer, MAX_PAYLOAD).await?;
            out.push(Signature { payload, signature: signature.clone() });
        }
        Ok(out)
    }

    /// Signs `name@digest` with `key` as `cosign sign --key` does, keeping every signature
    /// already there.
    pub async fn sign(
        &self,
        name: &str,
        digest: &str,
        key: &p256::ecdsa::SigningKey,
    ) -> Result<(), RegistryError> {
        let r = signature_ref(name, digest)?;
        let layer = |s: Signature| {
            let annotations = BTreeMap::from([(SIGNATURE_ANNOTATION.to_owned(), s.signature)]);
            ImageLayer::new(s.payload, SIMPLE_SIGNING_MEDIA_TYPE.to_owned(), Some(annotations))
        };
        let mut layers: Vec<ImageLayer> =
            self.signatures(name, digest).await?.into_iter().map(layer).collect();
        let payload = payload(name, digest);
        layers.push(layer(Signature { signature: sign(key, &payload), payload }));
        let config = Config::new(b"{}".to_vec(), EMPTY_CONFIG_MEDIA_TYPE.to_owned(), None);
        let manifest = OciImageManifest::build(&layers, &config, None);
        self.client
            .push(&r, &layers, config, &self.auth, Some(manifest))
            .await
            .map_err(|e| classify(&e))?;
        Ok(())
    }
}
