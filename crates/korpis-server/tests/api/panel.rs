// SPDX-License-Identifier: AGPL-3.0-or-later

//! The panel the tests sign in from: its origin, and the site its passkeys are bound to.

/// The origin the tests' panel is served from: passkeys are bound to `localhost`.
pub const PANEL: &str = "https://localhost:7443";

/// The relying party the tests' control plane is configured with.
pub fn relying_party() -> korpis_core::webauthn::RelyingParty {
    korpis_core::webauthn::RelyingParty { id: "localhost".into(), origins: vec![PANEL.into()] }
}
