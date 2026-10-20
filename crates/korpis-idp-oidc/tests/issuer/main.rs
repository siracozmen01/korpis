// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider against an independent OpenID Connect issuer, panva's oidc-provider
//! (`conformance/oidc`, installed by `scripts/test-tools.sh`), run as a host process; skipped
//! where it is not installed. A person signs in with PKCE and their code is asserted once; a
//! wrong nonce or verifier is refused; leaving a group is found by refreshing their token; two
//! sets asking at once spend it once (Finding 119 of docs/23-walkthroughs.md); and without
//! offline access the provider says it cannot tell.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod sign_in;
mod support;
