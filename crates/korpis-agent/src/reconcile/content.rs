// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's recipe content, which its backups record by reference rather than store (§5.2 of
//! docs/06-storage.md): the files of its recipe and what its install's `fetch`es downloaded, each
//! by its sha256 and size, as the node's content store holds them. Recipe content is public by
//! definition (§6 of docs/06-storage.md), so a file byte-identical to it costs a backup nothing.
//!
//! It only makes backups smaller: content the node cannot name, because the registry is down or
//! the store no longer holds it, is left out and backed up as any other file would be.

use super::Reconciler;
use super::act::Failure;
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_core::recipe::install::{Hash, InstallStep};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::backup::v1 as backup;
use std::collections::BTreeSet;

/// The 32 bytes `hex` spells, if it spells a sha256.
fn sha256_bytes(hex: &str) -> Option<Vec<u8>> {
    if hex.len() != 64 {
        return None;
    }
    (0..32)
        .map(|i| hex.get(2 * i..2 * i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
        .collect()
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// `a`'s recipe content as a backup job takes it; none where it cannot be named.
    pub(super) async fn recipe_content(&self, a: &agent::Assignment) -> Vec<backup::RecipeContent> {
        match self.named_content(a).await {
            Ok(content) => content,
            Err(e) => {
                tracing::warn!(workload = %a.workload, error = %e.0, "backed up without recipe content");
                Vec::new()
            }
        }
    }

    async fn named_content(
        &self,
        a: &agent::Assignment,
    ) -> Result<Vec<backup::RecipeContent>, Failure> {
        let fail = |e: &dyn std::fmt::Display| Failure(format!("recipe content: {e}"));
        let recipe = korpis_recipe::parse(&a.recipe_manifest).map_err(|e| fail(&e))?;
        let body = a.target.as_option().and_then(|i| i.body.as_option());
        let r = body.and_then(|b| b.recipe.as_option()).ok_or_else(|| fail(&"no recipe"))?;
        let files = self.host.fetch_files(&format!("{}@{}", r.name, r.digest)).await?;
        let mut digests: BTreeSet<String> = files.into_values().collect();
        digests.extend(recipe.install.iter().filter_map(|s| match s {
            InstallStep::Fetch { hash: Hash::Sha256(hex), .. } => Some(format!("sha256:{hex}")),
            _ => None,
        }));
        let mut out = Vec::new();
        for d in digests {
            let Some(sha256) = d.strip_prefix("sha256:").and_then(sha256_bytes) else { continue };
            if let Some(size) = self.host.content_size(&d).await? {
                out.push(backup::RecipeContent { size, sha256, ..Default::default() });
            }
        }
        Ok(out)
    }
}
