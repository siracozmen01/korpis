// SPDX-License-Identifier: AGPL-3.0-or-later

//! The delegation ceiling an organization sets (§5.1 of docs/08-identity.md): seven days where
//! none says, lowered by any organization for its subtree, raised only by the root, at most
//! ninety days and at least an hour, and every change an effect.

use crate::domains::child;
use crate::support::{Harness, mutation};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

const DAY: u64 = 86_400;

/// `n` days, in the seconds a ceiling reads back as.
const fn days(n: i64) -> i64 {
    n * 86_400
}

fn secs(d: Option<&buffa_types::google::protobuf::Duration>) -> Option<i64> {
    d.map(|d| d.seconds)
}

async fn read(h: &Harness, org: &str) -> (Option<i64>, Option<i64>) {
    let req =
        pb::GetDelegationCeilingRequest { organization: org.to_owned(), ..Default::default() };
    let got = h.orgs(h.root()).get_delegation_ceiling(req).await.unwrap().into_owned();
    let c = got.ceiling.into_option().unwrap();
    (secs(c.own.as_option()), secs(c.effective.as_option()))
}

async fn set(h: &Harness, org: &str, days: Option<u64>) -> Result<(), connectrpc::ConnectError> {
    let req = pb::SetDelegationCeilingRequest {
        mutation: mutation(&format!("ceiling-{}", uuid::Uuid::now_v7())).into(),
        organization: org.to_owned(),
        ceiling: days
            .map(|d| korpis_server::api::wire::duration(Duration::from_secs(d * DAY)))
            .into(),
        ..Default::default()
    };
    h.orgs(h.root()).set_delegation_ceiling(req).await.map(|_| ())
}

#[tokio::test]
async fn a_ceiling_is_lowered_beneath_and_raised_only_at_the_root() {
    let h = Harness::new().await;
    let root = h.root_org();
    let below = child(&h, "reseller").await;
    assert_eq!(read(&h, &below).await, (None, Some(days(7))), "seven days where none says");

    set(&h, &below, Some(1)).await.unwrap();
    assert_eq!(read(&h, &below).await, (Some(days(1)), Some(days(1))), "lowered");
    let above = set(&h, &below, Some(8)).await.unwrap_err();
    assert_eq!(above.code, ErrorCode::InvalidArgument, "only the root raises it");

    set(&h, &root, Some(30)).await.unwrap();
    assert_eq!(read(&h, &root).await, (Some(days(30)), Some(days(30))));
    set(&h, &below, Some(20)).await.unwrap();
    assert_eq!(read(&h, &below).await.1, Some(days(20)), "beneath the raised root");
    set(&h, &below, None).await.unwrap();
    assert_eq!(read(&h, &below).await, (None, Some(days(30))), "removed, the root's");

    let past = set(&h, &root, Some(91)).await.unwrap_err();
    assert_eq!(past.code, ErrorCode::InvalidArgument, "ninety days at most");
    assert_eq!(h.effects("org.delegation_ceiling.set").await.len(), 4, "every change");
}
