// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis workload classify`: the operator's layer of the classification chain (§3.3 of
//! docs/01-model.md), set as a whole, on the record, under the workload's version.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use korpis_proto::korpis::v1 as pb;

/// One `action=disruptive|routine` argument.
fn parse(given: &str) -> Result<pb::ClassificationOverride, CliError> {
    let refused = || CliError::Local(format!("{given:?}: action=disruptive or action=routine"));
    let (action, class) = given.split_once('=').ok_or_else(refused)?;
    let disruptive = match class {
        "disruptive" => true,
        "routine" => false,
        _ => return Err(refused()),
    };
    Ok(pb::ClassificationOverride { action: action.to_owned(), disruptive, ..Default::default() })
}

/// Replaces the overrides of workload `id` with `given`; none clears them.
pub async fn classify(
    c: &Connection,
    id: String,
    given: &[String],
    reason: &str,
) -> Result<Answer, CliError> {
    let overrides = given.iter().map(|g| parse(g)).collect::<Result<Vec<_>, _>>()?;
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let read = pb::GetWorkloadRequest { id: id.clone(), ..Default::default() };
    let status = client.get_workload(read).await?.into_owned().status;
    let version = status.as_option().and_then(|s| s.workload.as_option()).map_or(0, |w| w.version);
    let request = pb::SetClassificationRequest {
        mutation: mutation(Some(version)).into(),
        workload: id,
        overrides,
        reason: reason.to_owned(),
        ..Default::default()
    };
    let r = client.set_classification(request).await?.into_owned();
    let set = r.workload.as_option().map(|w| w.classification.clone()).unwrap_or_default();
    let human = if set.is_empty() {
        "classification: the driver's and the recipe's, with no override\n".to_owned()
    } else {
        let lines: Vec<String> = set
            .iter()
            .map(|o| {
                let class = if o.disruptive { "disruptive" } else { "routine" };
                format!("  {:<20} {class}\n", o.action)
            })
            .collect();
        format!("classification overrides:\n{}", lines.concat())
    };
    Ok(Answer { json: json(&r), human })
}
