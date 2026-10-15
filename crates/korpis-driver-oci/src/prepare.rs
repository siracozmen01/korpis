// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Prepare` (§4.2 of docs/04-runtimes.md): from an approved intent and the recipe it names, the
//! spec `Create` would build, or why this driver on this node cannot build it. It reads its
//! arguments and nothing else, so it changes nothing and answers the same every time. Volume
//! sources are left empty: where a volume lives is the helper's to say, and the agent fills
//! them in before `Create`, which validates the whole spec again.

use crate::validate::DEFAULT_PIDS;
use korpis_core::content::this_machine;
use korpis_core::recipe::runtime::{RuntimeTarget, TargetSource};
use korpis_core::value::{Value, ValueMap};
use korpis_core::workload::spec::IsolationTier;
use korpis_core::workload::spec::streams::Retention;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::value::Kind as V;

/// The name the driver answers to in an intent's `runtime.driver`.
pub const NAME: &str = "oci";

type Reason = pb::UnsatisfiableReason;

fn rejected(reason: Reason, detail: impl Into<String>) -> driver::Rejection {
    driver::Rejection { reason: reason.into(), detail: detail.into(), ..Default::default() }
}

/// A configuration value; a list or map, which no field delivered here holds, is refused later
/// by the delivery rules.
fn value_of(v: &pb::Value) -> Value {
    match &v.kind {
        Some(V::BoolValue(b)) => Value::Bool(*b),
        Some(V::IntValue(i)) => Value::Int(*i),
        Some(V::DoubleValue(d)) => Value::Double(*d),
        Some(V::StringValue(s)) => Value::String(s.clone()),
        Some(V::BytesValue(b)) => Value::Bytes(b.clone()),
        Some(V::ListValue(_) | V::MapValue(_)) => Value::List(Vec::new()),
        Some(V::NullValue(_)) | None => Value::Null,
    }
}

/// The recipe's container target for this machine's architecture: its image.
fn image_for(targets: &[RuntimeTarget]) -> Option<&str> {
    let (_, arch) = this_machine();
    targets.iter().find_map(|t| match &t.source {
        TargetSource::Image(image)
            if t.tier == IsolationTier::Container && t.arch.iter().any(|a| a == arch) =>
        {
            Some(image.as_str())
        }
        _ => None,
    })
}

/// That the intent asks for what this driver runs.
fn runs_here(body: &pb::IntentBody) -> Result<(), driver::Rejection> {
    let runtime = body.runtime.as_option();
    let name = runtime.map_or("", |r| r.driver.as_str());
    if !name.is_empty() && name != NAME {
        return Err(rejected(Reason::UNSATISFIABLE_REASON_MISSING_DRIVER, format!("{name:?}")));
    }
    let tier = runtime.and_then(|r| r.tier.as_known());
    if tier != Some(pb::IsolationTier::ISOLATION_TIER_CONTAINER) {
        let why = format!("the oci driver runs the container tier, not {tier:?}");
        return Err(rejected(Reason::UNSATISFIABLE_REASON_MISSING_DRIVER, why));
    }
    if runtime.is_some_and(|r| !r.config.is_empty()) {
        let why = "the oci driver declares no configuration of its own";
        return Err(rejected(Reason::UNSATISFIABLE_REASON_PREPARE_REJECTED, why));
    }
    Ok(())
}

/// The mounts of the intent's volumes, their sources left for the helper to name.
fn mounts(body: &pb::IntentBody) -> Vec<driver::Mount> {
    body.volumes
        .iter()
        .map(|v| driver::Mount {
            destination: v.path.clone(),
            read_only: v.mode.as_known() == Some(pb::VolumeMode::VOLUME_MODE_RO),
            volume: v.name.clone(),
            ..Default::default()
        })
        .collect()
}

/// The intent's resources, with the default process limit where it sets none (§5.2 of
/// docs/05-scheduling.md).
fn resources(body: &pb::IntentBody) -> pb::ResourceSpec {
    let mut r = body.resources.as_option().cloned().unwrap_or_default();
    if r.pids.as_option().is_none_or(|p| p.limit == 0) {
        r.pids = pb::PidsSpec { limit: DEFAULT_PIDS, ..Default::default() }.into();
    }
    r
}

/// The console stream's capture: its limits as the intent declares them, each left zero the
/// default, and the secret values it redacts; its directory is the agent's to fill. None unless
/// the console mode is one with a stream.
fn stream(body: &pb::IntentBody, secrets: Vec<(String, String)>) -> Option<driver::StreamCapture> {
    use pb::ConsoleMode as M;
    let mode = body.console.as_known();
    if !matches!(
        mode,
        Some(M::CONSOLE_MODE_LOGS | M::CONSOLE_MODE_INTERACTIVE | M::CONSOLE_MODE_SERIAL)
    ) {
        return None;
    }
    let limits = korpis_wire::stream::spec_from_pb("streams", body.streams.as_option()).ok()?;
    let (retention_bytes, retention_seconds) = match limits.retention {
        Retention::Bytes(b) => (b, 0),
        Retention::Duration(d) => (0, d.as_secs().max(1)),
    };
    Some(driver::StreamCapture {
        rate_bytes_per_sec: limits.rate,
        burst_bytes: limits.burst,
        retention_bytes,
        retention_seconds,
        input: mode == Some(M::CONSOLE_MODE_INTERACTIVE),
        secrets: secrets
            .into_iter()
            .map(|(name, value)| driver::StreamSecret { name, value, ..Default::default() })
            .collect(),
        ..Default::default()
    })
}

/// Prepares `intent` for `workload` from the recipe document `manifest`.
pub fn prepare(
    workload: &str,
    intent: &pb::Intent,
    manifest: &[u8],
) -> Result<driver::PreparedSpec, driver::Rejection> {
    let refuse = |why: String| rejected(Reason::UNSATISFIABLE_REASON_PREPARE_REJECTED, why);
    let body = intent.body.as_option().ok_or_else(|| refuse("the intent has no body".into()))?;
    runs_here(body)?;
    let recipe = korpis_recipe::parse(manifest).map_err(|e| refuse(e.to_string()))?;
    let image = image_for(&recipe.runtime.targets).ok_or_else(|| {
        let why = format!("the recipe has no container image for {}", this_machine().1);
        rejected(Reason::UNSATISFIABLE_REASON_IMAGE_UNSUPPORTED, why)
    })?;
    let values: ValueMap = body.config.iter().map(|(k, v)| (k.clone(), value_of(v))).collect();
    // Managed files are the agent's to render, into the directory it hands the driver as the
    // identity directory (§5.2 of docs/09-recipes.md); the driver sets only env and args.
    let delivered = recipe.config.delivered(&values).map_err(|e| refuse(e.to_string()))?;
    let stream = stream(body, delivered.secrets);
    Ok(driver::PreparedSpec {
        workload: workload.to_owned(),
        intent: intent.id.clone(),
        rootfs: image.to_owned(),
        command: recipe.runtime.entry.command,
        args: delivered.args,
        env: delivered.env.into_iter().collect(),
        working_dir: recipe.runtime.entry.working_dir,
        resources: resources(body).into(),
        mounts: mounts(body),
        tier: pb::IsolationTier::ISOLATION_TIER_CONTAINER.into(),
        stream: stream.into(),
        ..Default::default()
    })
}
