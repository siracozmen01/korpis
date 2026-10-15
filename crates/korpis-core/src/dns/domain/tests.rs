// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn name(s: &str) -> DomainName {
    DomainName::parse(s).unwrap()
}

fn held(org: u128, n: &str, verified: bool) -> Domain {
    Domain {
        id: Uuid::from_u128(org * 1000 + u128::try_from(n.len()).unwrap()),
        organization: Uuid::from_u128(org),
        name: name(n),
        verification: Verification::DnsTxt,
        token: String::new(),
        parent: None,
        verified_at: verified.then(SystemTime::now),
        created_at: SystemTime::now(),
    }
}

#[test]
fn a_name_is_lowercase_dotted_labels_without_a_wildcard() {
    for good in ["example.com", "shop.example.com", "xn--bcher-kva.example", "a-b.c1.io"] {
        assert!(DomainName::parse(good).is_some(), "{good}");
    }
    for bad in
        ["com", "Example.com", "*.example.com", "example.com.", "-a.com", "a..com", "_x.com", ""]
    {
        assert!(DomainName::parse(bad).is_none(), "{bad}");
    }
    assert_eq!(name("example.com").verification_record(), "_korpis-verify.example.com");
}

#[test]
fn a_name_covers_itself_and_what_lies_beneath_it_and_nothing_that_merely_ends_alike() {
    let apex = name("example.com");
    assert!(apex.covers(&name("example.com")) && apex.covers(&name("shop.example.com")));
    assert!(!apex.covers(&name("badexample.com")), "a suffix that is not a label");
    assert!(!name("shop.example.com").covers(&apex));
    assert!(name("shop.example.com").overlaps(&apex));
}

#[test]
fn another_organizations_verified_domain_blocks_unless_it_delegated() {
    let theirs = held(1, "example.com", true);
    let pending = held(3, "other.com", false);
    let mine = held(2, "shop.example.com", false);
    let others = [theirs.clone(), pending];
    assert_eq!(blocked_by(&mine, &others, &[]).map(|d| d.id), Some(theirs.id));
    assert!(blocked_by(&mine, &others, &[theirs.id]).is_none(), "delegated beneath it");
    assert!(
        blocked_by(&held(2, "other.com", false), &others, &[]).is_none(),
        "pending blocks no one"
    );
    assert!(blocked_by(&held(1, "a.example.com", false), &others, &[]).is_none(), "its own");
}

#[test]
fn the_most_specific_verified_domain_decides_who_routes_a_name() {
    let verified = [held(1, "example.com", true), held(2, "shop.example.com", true)];
    assert_eq!(holder_of(&name("www.example.com"), &verified), Some(Uuid::from_u128(1)));
    assert_eq!(holder_of(&name("a.shop.example.com"), &verified), Some(Uuid::from_u128(2)));
    assert_eq!(holder_of(&name("other.org"), &verified), None);
}
