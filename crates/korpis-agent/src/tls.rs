// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whom the agent trusts: the installation CA, pinned by fingerprint when it joins and trusted
//! alone afterwards (`korpis-tls`).

pub use korpis_tls::{Pinned, TlsError, fingerprint, pinned, trusting};
