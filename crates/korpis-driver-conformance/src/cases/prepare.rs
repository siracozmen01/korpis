// SPDX-License-Identifier: Apache-2.0

//! `Describe` and `Prepare`: a driver says what it is when asked, and `Prepare` changes nothing,
//! answers the same each time, and refuses with a rejection, not an error, what this driver on
//! this node cannot run (§4.1 and §4.2 of docs/04-runtimes.md).

use super::Stopped;
use crate::case::Verdict;
use crate::probe;
use crate::suite::Suite;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;

/// Its name, its protocol, the tiers it runs, and its health are said.
pub fn describes_itself(s: &Suite) -> Verdict {
    let i = &s.info;
    if i.name.is_empty() || i.protocol.is_empty() {
        return Verdict::fail(format!(
            "a name and a protocol, not {:?} and {:?}",
            i.name, i.protocol
        ));
    }
    if s.capabilities().tiers.is_empty() {
        return Verdict::fail("no tier declared");
    }
    Verdict::check(i.healthy, "it describes itself unhealthy")
}

/// The objects it holds, each by id with the workload its labels name.
async fn held(s: &Suite) -> Result<Vec<(String, String)>, Stopped> {
    let objects = s.driver().recover().await.map_err(|e| format!("Recover: {e}"))?;
    let mut held: Vec<(String, String)> = objects
        .into_iter()
        .map(|o| {
            let workload = o.labels.as_option().map(|l| l.workload.clone()).unwrap_or_default();
            (o.runtime_id, workload)
        })
        .collect();
    held.sort();
    Ok(held)
}

/// Two `Prepare`s of one intent answer the same, and the driver holds no object it did not hold
/// before for the workload prepared, or for none: the objects of other workloads, which a node's
/// other users may make and remove meanwhile, are not the case's to judge.
pub async fn pure(s: &Suite) -> Result<Verdict, Stopped> {
    let before = held(s).await?;
    let (workload, intent) = s.workload();
    let recipe = s.recipe(&probe::SERVE);
    let first = s.driver().prepare(&workload, &intent, &recipe).await.map_err(|e| e.to_string())?;
    let second =
        s.driver().prepare(&workload, &intent, &recipe).await.map_err(|e| e.to_string())?;
    if !matches!(first.result, Some(driver::prepare_response::Result::Prepared(_))) {
        return Ok(Verdict::fail(format!("the probe was not prepared: {first:?}")));
    }
    if first != second {
        return Ok(Verdict::fail("two Prepares of one intent answered differently"));
    }
    let left = held(s).await?.into_iter().any(|(id, of)| {
        (of == workload || of.is_empty()) && !before.iter().any(|(was, _)| *was == id)
    });
    Ok(Verdict::check(!left, "Prepare left an object behind"))
}

/// Whether `answer` is a rejection, and not a prepared spec or an error.
fn rejected(answer: driver::PrepareResponse) -> Verdict {
    match answer.result {
        Some(driver::prepare_response::Result::Rejection(r)) if !r.detail.is_empty() => {
            Verdict::Pass
        }
        Some(driver::prepare_response::Result::Rejection(_)) => {
            Verdict::fail("a rejection that does not say why")
        }
        other => Verdict::fail(format!("not a rejection: {other:?}")),
    }
}

/// An intent at a tier the driver does not declare is rejected.
pub async fn refuses_another_tier(s: &Suite) -> Result<Verdict, Stopped> {
    let declared = s.capabilities().tiers;
    let all = [
        pb::IsolationTier::ISOLATION_TIER_PROCESS,
        pb::IsolationTier::ISOLATION_TIER_CONTAINER,
        pb::IsolationTier::ISOLATION_TIER_MICROVM,
        pb::IsolationTier::ISOLATION_TIER_VM,
    ];
    let Some(other) = all.into_iter().find(|t| !declared.iter().any(|d| d.as_known() == Some(*t)))
    else {
        return Ok(Verdict::NotApplicable("it declares every tier".into()));
    };
    let (workload, mut intent) = s.workload();
    if let Some(r) = intent.body.as_option_mut().and_then(|b| b.runtime.as_option_mut()) {
        r.tier = other.into();
    }
    let answer = s.driver().prepare(&workload, &intent, &s.recipe(&probe::SERVE)).await;
    Ok(rejected(answer.map_err(|e| format!("Prepare answered an error: {e}"))?))
}

/// An intent naming another driver is rejected.
pub async fn refuses_another_driver(s: &Suite) -> Result<Verdict, Stopped> {
    let (workload, mut intent) = s.workload();
    if let Some(r) = intent.body.as_option_mut().and_then(|b| b.runtime.as_option_mut()) {
        r.driver = format!("not-{}", s.info.name);
    }
    let answer = s.driver().prepare(&workload, &intent, &s.recipe(&probe::SERVE)).await;
    Ok(rejected(answer.map_err(|e| format!("Prepare answered an error: {e}"))?))
}
