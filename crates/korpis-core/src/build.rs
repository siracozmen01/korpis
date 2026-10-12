// SPDX-License-Identifier: AGPL-3.0-or-later

//! Build provenance.

/// The package version of this build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The exact source revision this build was made from, or `unknown` if it was built outside a
/// git checkout without `KORPIS_SOURCE_REVISION` set.
pub const SOURCE_REVISION: &str = env!("KORPIS_SOURCE_REVISION");

/// Where the corresponding source is published. AGPL-3.0 sections 4 to 6 attach to conveyed
/// copies, and every build meets them by naming this and [`SOURCE_REVISION`].
pub const SOURCE_URL: &str = "https://github.com/siracozmen01/korpis";

/// One line naming a binary, its version, and its source, for every `--version` output.
#[must_use]
pub fn describe(binary: &str) -> String {
    format!("{binary} {VERSION} (source {SOURCE_URL} at {SOURCE_REVISION})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_names_the_source() {
        let line = describe("korpis");
        assert!(line.starts_with("korpis "));
        assert!(line.contains(SOURCE_URL));
        assert!(line.contains(SOURCE_REVISION));
    }
}
