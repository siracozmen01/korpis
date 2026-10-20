// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in through the issuer, and re-verifying who signed in.

use crate::support::{Issuer, headers, provider, sign_in};
use korpis_extension::identity_provider::{Answer, Asked, IdentityProvider, Set};

fn group(name: &str) -> Set {
    Set { realm: "groups".into(), member_of: name.into() }
}

fn asked(about: Option<Set>) -> Asked {
    Asked { set: about, members: vec!["dana".into()], require_membership: Vec::new() }
}

fn found(a: &Answer) -> (&[String], &[String]) {
    (&a.present, &a.unknown)
}

#[tokio::test]
async fn a_person_signs_in_once_and_leaving_a_group_is_found_by_one_refresh() {
    let Some(issuer) = Issuer::start("ES256").await else { return };
    let state = tempfile::tempdir().unwrap();
    let oidc = provider(issuer.settings(Some(state.path())));
    let returned = sign_in(&oidc).await;
    let code = returned.code.clone().into_bytes();
    let person = oidc.assert(code, headers(&returned)).await.unwrap();
    assert_eq!(person.user, "dana");
    assert_eq!(person.sets, [group("ops"), group("oncall")]);

    let ops = oidc.verify(asked(Some(group("ops")))).await.unwrap();
    assert!(ops.set_exists);
    assert_eq!(found(&ops), (&["dana".to_owned()][..], &[][..]));

    issuer.set("dana", &[("dana", &["oncall"])]);
    let fresh = provider(issuer.settings(Some(state.path())));
    let left = fresh.verify(asked(Some(group("ops")))).await.unwrap();
    assert_eq!(found(&left), (&[][..], &[][..]), "left the group, and said so");

    // Two sets ask about her at once: one refresh answers both, so the issuer never sees her
    // rotated token twice and never revokes her grant (Finding 119).
    let both = provider(issuer.settings(Some(state.path())));
    let (ops, oncall) = tokio::join!(
        both.verify(asked(Some(group("ops")))),
        both.verify(asked(Some(group("oncall"))))
    );
    assert!(ops.unwrap().present.is_empty());
    assert_eq!(oncall.unwrap().present, ["dana"]);
    let after = provider(issuer.settings(Some(state.path())));
    assert_eq!(after.verify(asked(None)).await.unwrap().present, ["dana"], "her grant stands");

    issuer.set("dana", &[]);
    let gone = provider(issuer.settings(Some(state.path())));
    assert_eq!(found(&gone.verify(asked(None)).await.unwrap()), (&[][..], &[][..]));
    let other = gone.verify(asked(Some(group("elsewhere:x")))).await.unwrap();
    assert!(!other.set_exists || other.present.is_empty());
}

#[tokio::test]
async fn a_code_redeemed_twice_is_refused_and_ends_what_it_gave() {
    let Some(issuer) = Issuer::start("ES256").await else { return };
    let state = tempfile::tempdir().unwrap();
    let oidc = provider(issuer.settings(Some(state.path())));
    let returned = sign_in(&oidc).await;
    let code = returned.code.clone().into_bytes();
    oidc.assert(code.clone(), headers(&returned)).await.unwrap();
    let again = oidc.assert(code, headers(&returned)).await.unwrap_err();
    assert_eq!(again.reason, "identity.unverified", "a code is redeemed once");
    // The issuer takes the second redemption as theft and revokes the first's refresh token
    // (RFC 6749 §4.1.2), which is why the control plane spends a sign-in's state before it
    // ever asks for the code (§9.7 of docs/08-identity.md).
    let later = provider(issuer.settings(Some(state.path())));
    assert_eq!(found(&later.verify(asked(None)).await.unwrap()), (&[][..], &[][..]));
}

#[tokio::test]
async fn a_code_returned_for_another_sign_in_is_refused() {
    let Some(issuer) = Issuer::start("RS256").await else { return };
    let oidc = provider(issuer.settings(None));
    let returned = sign_in(&oidc).await;
    let mut wrong_nonce = headers(&returned);
    wrong_nonce.insert("nonce".into(), "another".into());
    let refused = oidc.assert(returned.code.clone().into_bytes(), wrong_nonce).await;
    assert_eq!(refused.unwrap_err().reason, "identity.unverified");
    let returned = sign_in(&oidc).await;
    let mut wrong_verifier = headers(&returned);
    wrong_verifier.insert("code_verifier".into(), "x".repeat(43));
    let refused = oidc.assert(returned.code.into_bytes(), wrong_verifier).await;
    assert_eq!(refused.unwrap_err().reason, "identity.unverified", "PKCE binds the code");
}

#[tokio::test]
async fn without_offline_access_a_person_is_neither_confirmed_nor_ended() {
    let Some(issuer) = Issuer::start("EdDSA").await else { return };
    let oidc = provider(issuer.settings(None));
    let returned = sign_in(&oidc).await;
    let person = oidc.assert(returned.code.clone().into_bytes(), headers(&returned)).await;
    assert_eq!(person.unwrap().user, "dana", "an Ed25519 token verifies");
    let ops = oidc.verify(asked(Some(group("ops")))).await.unwrap();
    assert_eq!(found(&ops), (&[][..], &["dana".to_owned()][..]), "it cannot tell");
}

#[tokio::test]
async fn a_client_the_issuer_refuses_ends_nobody_and_keeps_their_token() {
    let Some(issuer) = Issuer::start("ES256").await else { return };
    let state = tempfile::tempdir().unwrap();
    let oidc = provider(issuer.settings(Some(state.path())));
    let returned = sign_in(&oidc).await;
    oidc.assert(returned.code.clone().into_bytes(), headers(&returned)).await.unwrap();
    // The operator rotated the client's secret at the issuer before at the provider: every
    // refresh is refused `invalid_client`, which says nothing of the person.
    let mut rotated = issuer.settings(Some(state.path()));
    rotated.client_secret = "a-secret-the-issuer-no-longer-holds".into();
    let wrong = provider(rotated);
    let refused = wrong.verify(asked(Some(group("ops")))).await.unwrap();
    assert_eq!(found(&refused), (&[][..], &["dana".to_owned()][..]), "it cannot tell");
    let right = provider(issuer.settings(Some(state.path())));
    let kept = right.verify(asked(Some(group("ops")))).await.unwrap();
    assert_eq!(kept.present, ["dana"], "her token was kept, and still works");
}
