// SPDX-License-Identifier: AGPL-3.0-or-later

//! How an intent names a recipe. The digest is the identity; name and version are the pointer
//! that resolved to it at declaration (§2 and §8 of docs/09-recipes.md).

/// A resolved recipe reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecipeRef {
    /// The OCI repository, e.g. `korpis.io/minecraft/paper`.
    pub name: String,
    /// The tag that was resolved, e.g. `1.21.4`.
    pub version: String,
    /// `sha256:` and 64 lowercase hexadecimal digits.
    pub digest: String,
    /// What vouched for the recipe when it was declared, under the declaring organization's
    /// policy (§7 of docs/09-recipes.md); `None` only in a declaration not yet resolved.
    pub trust: Option<crate::recipe::trust::Vouched>,
}

/// Why a reference is malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefError {
    /// The repository name is empty or contains whitespace or an `@`.
    #[error("recipe name {0:?} is not an OCI repository name")]
    Name(String),
    /// The digest is not `sha256:` and 64 lowercase hex digits.
    #[error("recipe digest {0:?} is not sha256:<64 hex digits>")]
    Digest(String),
}

/// Whether `d` is a sha256 digest in canonical form.
#[must_use]
pub fn is_digest(d: &str) -> bool {
    d.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

impl RecipeRef {
    /// Checks the reference's form. Whether the digest is what the name resolves to is the
    /// resolver's question, not this one's.
    pub fn validate(&self) -> Result<(), RefError> {
        let bad_name = self.name.is_empty()
            || self.name.chars().any(|c| c.is_whitespace() || c == '@' || c.is_control());
        if bad_name {
            return Err(RefError::Name(self.name.clone()));
        }
        if !is_digest(&self.digest) {
            return Err(RefError::Digest(self.digest.clone()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_are_canonical_sha256() {
        let ok = format!("sha256:{}", "a".repeat(64));
        assert!(is_digest(&ok));
        assert!(!is_digest(&format!("sha256:{}", "A".repeat(64))));
        assert!(!is_digest(&format!("sha512:{}", "a".repeat(64))));
        assert!(!is_digest("sha256:abc"));
        let r =
            RecipeRef { name: "korpis.io/x".into(), version: "1".into(), digest: ok, trust: None };
        assert_eq!(r.validate(), Ok(()));
        let named = RecipeRef { name: "x@y".into(), ..r };
        assert!(named.validate().is_err());
    }
}
