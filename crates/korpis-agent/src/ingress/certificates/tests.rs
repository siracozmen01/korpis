// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use korpis_proto::korpis::v1 as pb;
use std::os::unix::fs::PermissionsExt as _;

fn asked(rx: &mut mpsc::Receiver<agent::AgentMessage>) -> Option<agent::CertificateRequest> {
    match rx.try_recv().ok()?.message {
        Some(Up::CertificateRequest(r)) => Some(*r),
        _ => None,
    }
}

fn served(request: &str, id: &str, revision: i64) -> agent::CertificateKey {
    let c = agent::ServedCertificate {
        id: id.into(),
        revision,
        chain: "chain".into(),
        key: "key".into(),
        ..Default::default()
    };
    agent::CertificateKey {
        request: request.into(),
        outcome: Some(Outcome::Certificate(Box::new(c))),
        ..Default::default()
    }
}

#[test]
fn a_certificate_is_asked_for_once_kept_on_disk_and_asked_again_at_a_new_revision() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel(8);
    let certs = Certificates::open(dir.path().join("certificates"), tx.clone());
    certs.want("crt_1", 1);
    let first = asked(&mut rx).expect("asked");
    certs.want("crt_1", 1);
    assert!(asked(&mut rx).is_none(), "a question stands");
    certs.answer(served(&first.request, "crt_1", 1));
    certs.want("crt_1", 1);
    assert!(asked(&mut rx).is_none(), "held at that revision");
    let path = dir.path().join("certificates/crt_1.binpb");
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

    let again = Certificates::open(dir.path().join("certificates"), tx);
    let wanted = BTreeSet::from(["crt_1".to_owned()]);
    assert_eq!(again.serve(&wanted)[0].key, "key", "kept across a restart");
    again.want("crt_1", 2);
    assert_eq!(asked(&mut rx).unwrap().certificate, "crt_1", "a new revision is asked for");
}

#[test]
fn a_refusal_waits_and_what_is_no_longer_served_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel(8);
    let certs = Certificates::open(dir.path().to_owned(), tx);
    certs.want("crt_1", 1);
    let q = asked(&mut rx).unwrap();
    let refusal = pb::Error { reason: "certificate.pending".into(), ..Default::default() };
    certs.answer(agent::CertificateKey {
        request: q.request,
        outcome: Some(Outcome::Refused(Box::new(refusal))),
        ..Default::default()
    });
    certs.want("crt_1", 1);
    assert!(asked(&mut rx).is_none(), "asked again only after a while");

    certs.want("crt_2", 1);
    let q = asked(&mut rx).unwrap();
    certs.answer(served(&q.request, "crt_2", 1));
    certs.answer(served("unasked", "crt_3", 1));
    assert_eq!(certs.serve(&BTreeSet::from(["crt_2".to_owned()])).len(), 1);
    assert!(certs.serve(&BTreeSet::new()).is_empty());
    assert!(!dir.path().join("crt_2.binpb").exists(), "its key is gone from disk");
}
