// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recovery codes (§9.3 of docs/08-identity.md): ten, shown once at enrolment, each a second
//! factor that works once. A code is 80 random bits, written as sixteen base32 characters in
//! four groups, and stored as its SHA-256 digest: with that much entropy a slow hash protects
//! nothing, as for an API token, and checking one code against ten stored ones stays cheap.

use super::{EntropyError, base32, random};
use sha2::{Digest, Sha256};

/// How many codes an account is given at once.
pub const CODES: usize = 10;

/// A fresh set of codes, as shown once.
pub fn generate() -> Result<Vec<String>, EntropyError> {
    (0..CODES)
        .map(|_| {
            let raw = base32(&random::<10>()?);
            let groups: Vec<&str> = (0..4).filter_map(|i| raw.get(i * 4..i * 4 + 4)).collect();
            Ok(groups.join("-"))
        })
        .collect()
}

/// The digest a code is stored as. A code is compared without its dashes and in upper case, so
/// it can be typed as read.
#[must_use]
pub fn digest(code: &str) -> [u8; 32] {
    let normal: String = code
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    Sha256::digest(normal.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_ten_distinct_groups_of_base32_compared_as_typed() {
        let codes = generate().unwrap();
        assert_eq!(codes.len(), CODES);
        assert!(codes.iter().all(|c| c.len() == 19 && c.matches('-').count() == 3), "{codes:?}");
        let distinct: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(distinct.len(), CODES);
        let typed = codes[0].to_lowercase().replace('-', " ");
        assert_eq!(digest(&typed), digest(&codes[0]));
        assert_ne!(digest(&codes[1]), digest(&codes[0]));
    }
}
