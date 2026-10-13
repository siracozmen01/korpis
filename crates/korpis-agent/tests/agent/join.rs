// SPDX-License-Identifier: AGPL-3.0-or-later

//! Joining (§3 of docs/18-operations.md): over a connection verified against the pinned CA,
//! with a key the node generated, recorded once.

use crate::support::ControlPlane;
use korpis_agent::join::{Join, JoinError, join};
use korpis_agent::state::{StateDir, StateError};
use korpis_agent::tls::fingerprint;
use rustls::pki_types::pem::PemObject as _;

fn request(cp: &ControlPlane, token: &str, ca: &str) -> Join {
    Join {
        server: cp.url.clone(),
        token: token.to_owned(),
        ca_fingerprint: ca.to_owned(),
        hostname: "fra-3".into(),
    }
}

#[tokio::test]
async fn a_node_joins_once_over_the_pinned_ca_and_keeps_its_identity() {
    let cp = ControlPlane::start().await;
    let (token, ca) = cp.token("fra-3").await;
    let dir = tempfile::tempdir().unwrap();
    let state = StateDir::new(dir.path().join("agent"));

    let wrong = "sha256:".to_owned() + &"0".repeat(64);
    let refused = join(&state, &request(&cp, &token, &wrong)).await.unwrap_err();
    assert!(matches!(refused, JoinError::Enroll(_)), "an unpinned CA is not trusted: {refused}");

    let id = join(&state, &request(&cp, &token, &ca)).await.unwrap();
    assert!(id.node.starts_with("nd_"));
    let loaded = state.load().unwrap();
    assert_eq!(
        (loaded.node.as_str(), loaded.key.as_bytes()),
        (id.node.as_str(), id.key.as_bytes())
    );
    let pem = String::from_utf8(loaded.ca_pem.clone()).unwrap();
    let der = rustls::pki_types::CertificateDer::from_pem_slice(pem.as_bytes()).unwrap();
    assert_eq!(fingerprint(der.as_ref()), ca, "the CA kept is the CA pinned");
    assert!(!loaded.keys.assignment_keys.is_empty(), "the keys that sign assignments");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode =
            std::fs::metadata(dir.path().join("agent/node.key")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the identity key is the owner's alone");
    }
    let again = join(&state, &request(&cp, &token, &ca)).await.unwrap_err();
    assert!(matches!(again, JoinError::State(StateError::AlreadyEnrolled(_))));
}
