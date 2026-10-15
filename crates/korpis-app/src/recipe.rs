// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resolving a recipe (§8 of docs/09-recipes.md): `name:version` becomes a digest once, at
//! declaration, and from then on the digest is the recipe's identity. The store keeps every
//! recipe ever resolved, so a workload never depends on a registry still holding what it runs.

use crate::App;
use crate::error::AppError;
use crate::ports::{CachedRecipe, Clock, Described, Fetched, RecipeSource, SourceError, Store};
use korpis_core::recipe::reference::is_digest;
use korpis_core::recipe::trust::cosign::verified;
use korpis_core::recipe::trust::{Signature, TrustPolicy, Untrusted, Vouched};
use korpis_core::recipe::{Recipe, RecipeRef};
use std::future::Future;

fn source_error(e: SourceError) -> AppError {
    match e {
        SourceError::NotFound(_) => AppError::NotFound { kind: "recipe" },
        SourceError::Invalid(m) => AppError::invalid("recipe", m),
        SourceError::Unavailable(detail) => {
            AppError::Unavailable { what: "the recipe registry", detail }
        }
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The digest `name` at `version` names: `version` itself when it is one.
    async fn digest_of(&self, name: &str, version: &str) -> Result<String, AppError> {
        if is_digest(version) {
            Ok(version.to_owned())
        } else {
            self.recipes.resolve(name, version).await.map_err(source_error)
        }
    }

    /// The recipe `name` at `version`, which may be a tag or a digest, with no trust decision:
    /// for reading what an approved intent already names.
    pub async fn resolve_recipe(
        &self,
        name: &str,
        version: &str,
    ) -> Result<(RecipeRef, Recipe), AppError> {
        let digest = self.digest_of(name, version).await?;
        let (reference, described) = self.pinned(name, version, digest).await?;
        Ok((reference, described.recipe))
    }

    /// What the document with `digest` describes, from the store or else fetched and kept.
    async fn pinned(
        &self,
        name: &str,
        version: &str,
        digest: String,
    ) -> Result<(RecipeRef, Described), AppError> {
        let reference = RecipeRef {
            name: name.to_owned(),
            version: version.to_owned(),
            digest,
            trust: None,
            providers: Vec::new(),
        };
        reference.validate().map_err(|e| AppError::invalid("recipe", e.to_string()))?;
        if let Some(cached) = self.store.cached_recipe(&reference.digest).await? {
            let described = self.recipes.parse(&cached.document).map_err(source_error)?;
            return Ok((reference, described));
        }
        let Fetched { document, described } =
            self.recipes.fetch(name, &reference.digest).await.map_err(source_error)?;
        let kept = CachedRecipe { name: name.to_owned(), version: version.to_owned(), document };
        self.store.cache_recipe(&reference.digest, &kept).await?;
        Ok((reference, described))
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// What `name` at `version` describes, resolved for a workload of `organization` under the
    /// trust policy that governs it (§7 of docs/09-recipes.md), and recorded with what vouched
    /// for it. The policy is decided on the digest and its signatures before anything of the
    /// recipe is fetched; a recipe the store already holds is decided on the signatures kept
    /// with it, and the registry is asked again only if those do not suffice.
    pub async fn resolve_trusted(
        &self,
        name: &str,
        version: &str,
        organization: uuid::Uuid,
    ) -> Result<(RecipeRef, Described), AppError> {
        let digest = self.digest_of(name, version).await?;
        let policy = self.store.trust_policy(organization).await?;
        let policy = policy.map(|p| p.policy).unwrap_or_default();
        let vouched = self.vouch(name, &digest, &policy).await?;
        let (mut reference, described) = self.pinned(name, version, digest).await?;
        reference.trust = Some(vouched);
        Ok((reference, described))
    }

    async fn vouch(
        &self,
        name: &str,
        digest: &str,
        policy: &TrustPolicy,
    ) -> Result<Vouched, AppError> {
        let kept = self.store.cached_signatures(digest).await?;
        let decided = policy.decide(digest, &verified(digest, &kept, &policy.accepted_signers));
        let signed = matches!(decided, Ok(Vouched::Signer(_)));
        if signed || policy.accepted_signers.is_empty() {
            return decided.map_err(|e| untrusted(&e));
        }
        // Signatures may have been added since the recipe was first seen. A registry out of
        // reach leaves the decision the kept ones gave, if they gave one.
        let found = match self.recipes.signatures(name, digest).await {
            Ok(found) => found,
            Err(SourceError::Unavailable(_)) if decided.is_ok() => {
                return decided.map_err(|e| untrusted(&e));
            }
            Err(e) => return Err(source_error(e)),
        };
        self.store.cache_signatures(digest, &found).await?;
        policy
            .decide(digest, &verified(digest, &found, &policy.accepted_signers))
            .map_err(|e| untrusted(&e))
    }
}

fn untrusted(e: &Untrusted) -> AppError {
    AppError::Untrusted { message: e.to_string() }
}

/// A source for an application that resolves no recipes: every request is refused as
/// unavailable. For compositions that do not declare workloads.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRecipes;

impl RecipeSource for NoRecipes {
    fn resolve(
        &self,
        _: &str,
        _: &str,
    ) -> impl Future<Output = Result<String, SourceError>> + Send {
        std::future::ready(Err(SourceError::Unavailable("no recipe source is configured".into())))
    }

    fn fetch(&self, _: &str, _: &str) -> impl Future<Output = Result<Fetched, SourceError>> + Send {
        std::future::ready(Err(SourceError::Unavailable("no recipe source is configured".into())))
    }

    fn signatures(
        &self,
        _: &str,
        _: &str,
    ) -> impl Future<Output = Result<Vec<Signature>, SourceError>> + Send {
        std::future::ready(Err(SourceError::Unavailable("no recipe source is configured".into())))
    }

    fn parse(&self, _: &[u8]) -> Result<Described, SourceError> {
        Err(SourceError::Unavailable("no recipe source is configured".into()))
    }
}
