// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn name(s: &str) -> DomainName {
    DomainName::parse(s).unwrap()
}

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
}

fn leaf(names: &[&str]) -> Leaf {
    Leaf {
        names: names.iter().map(|n| (*n).to_owned()).collect(),
        not_before: at(1000),
        not_after: at(4000),
        key_id: "k1".into(),
    }
}

#[test]
fn a_wildcard_stands_for_exactly_one_label() {
    assert!(matches("*.example.com", &name("shop.example.com")));
    assert!(!matches("*.example.com", &name("example.com")));
    assert!(!matches("*.example.com", &name("a.shop.example.com")));
    assert!(!matches("*.example.com", &name("shop.example.org")));
    assert!(matches("shop.example.com", &name("shop.example.com")));
    assert!(!matches("shop.example.com", &name("www.shop.example.com")));
}

#[test]
fn a_leaf_names_dns_names_and_has_not_expired() {
    assert_eq!(leaf(&["shop.example.com", "*.example.com"]).check(at(2000)), Ok(()));
    assert_eq!(leaf(&[]).check(at(2000)), Err(LeafError::Names(0)));
    assert_eq!(leaf(&["203.0.113.7"]).check(at(2000)), Err(LeafError::Name("203.0.113.7".into())));
    assert_eq!(
        leaf(&["*.*.example.com"]).check(at(2000)),
        Err(LeafError::Name("*.*.example.com".into()))
    );
    assert_eq!(
        leaf(&["Shop.example.com"]).check(at(2000)),
        Err(LeafError::Name("Shop.example.com".into()))
    );
    assert_eq!(leaf(&["shop.example.com"]).check(at(4000)), Err(LeafError::Expired));
    let many: Vec<String> = (0..=MAX_NAMES).map(|i| format!("n{i}.example.com")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(leaf(&many).check(at(2000)), Err(LeafError::Names(MAX_NAMES + 1)));
}

#[test]
fn a_certificate_serves_the_names_it_covers_while_valid() {
    let c = Certificate {
        id: Uuid::nil(),
        organization: Uuid::nil(),
        names: vec!["*.example.com".into(), "example.com".into()],
        source: Source::Provided,
        chain: String::new(),
        not_before: Some(at(1000)),
        not_after: Some(at(4000)),
        state: State::Issued,
        reason: None,
        renew_after: None,
        revision: 1,
        created_at: at(1000),
        owner: None,
        attempts: 0,
        next_attempt: None,
        key_id: Some("k1".into()),
        exposed: false,
    };
    let wanted = [name("example.com"), name("shop.example.com"), name("a.b.example.com")];
    assert_eq!(c.uncovered(&wanted), Some(&wanted[2]));
    assert_eq!(c.uncovered(&wanted[..2]), None);
    assert!(c.valid_at(at(1000)) && c.valid_at(at(3999)));
    assert!(!c.valid_at(at(999)) && !c.valid_at(at(4000)));
    assert!(!Certificate { state: State::Pending, ..c }.valid_at(at(2000)));
}

#[test]
fn failed_attempts_back_off_from_five_minutes_to_a_day() {
    let minutes = |n| backoff(n).as_secs() / 60;
    assert_eq!([minutes(1), minutes(2), minutes(3), minutes(4)], [5, 10, 20, 40]);
    assert_eq!(minutes(0), 5, "the first failure waits too");
    assert_eq!(backoff(30), Duration::from_secs(86_400));
}

#[test]
fn expiry_is_told_in_the_certificates_own_life() {
    use crate::event::Severity;
    let day = |n: u64| at(n * 86_400);
    // Ninety days: renewed at sixty, told at seventy-five, critical at eighty-five.
    assert_eq!(expiry_alert(day(0), day(90), day(74)), None);
    assert_eq!(expiry_alert(day(0), day(90), day(75)), Some(Severity::Warning));
    assert_eq!(expiry_alert(day(0), day(90), day(85)), Some(Severity::Critical));
    assert_eq!(expiry_alert(day(0), day(90), day(91)), Some(Severity::Critical), "expired");
    // Six days is not told all its life.
    assert_eq!(expiry_alert(day(0), day(6), day(4)), None);
}

#[test]
fn renewal_begins_two_thirds_through_a_certificates_life() {
    assert_eq!(renew_after(at(0), at(90 * 86_400)), at(60 * 86_400));
    assert_eq!(renew_after(at(10), at(5)), at(10), "a life that ends before it begins");
}
