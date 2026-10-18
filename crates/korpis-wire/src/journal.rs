// SPDX-License-Identifier: AGPL-3.0-or-later

//! Journal records: `korpis.agent.v1.JournalRecord` (§4.10 of docs/02-architecture.md). What a
//! node reports is translated strictly; whether to believe it is the ingesting use case's. A
//! record that does not translate is still a record: it keeps its sequence number and becomes a
//! malformed one, so the cursor passes it and one bad record cannot block a journal.

use crate::{Result, id, invalid, time};
use korpis_core::audit::Outcome;
use korpis_core::id::IdKind;
use korpis_core::node::journal::{Body, NodeEffect, Record};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::value::Kind as V;
use serde_json::{Map, Number, Value};

/// A `korpis.v1.Value` as JSON; bytes become an array of numbers, a non-finite double null.
#[must_use]
pub fn json_from_pb(v: &pb::Value) -> Value {
    match v.kind.as_ref() {
        None | Some(V::NullValue(_)) => Value::Null,
        Some(V::BoolValue(b)) => Value::Bool(*b),
        Some(V::IntValue(i)) => Value::from(*i),
        Some(V::DoubleValue(d)) => Number::from_f64(*d).map_or(Value::Null, Value::Number),
        Some(V::StringValue(s)) => Value::String(s.clone()),
        Some(V::BytesValue(b)) => Value::Array(b.iter().map(|x| Value::from(*x)).collect()),
        Some(V::ListValue(l)) => Value::Array(l.values.iter().map(json_from_pb).collect()),
        Some(V::MapValue(m)) => Value::Object(json_map(&m.fields)),
    }
}

fn json_map<'a>(m: impl IntoIterator<Item = (&'a String, &'a pb::Value)>) -> Map<String, Value> {
    m.into_iter().map(|(k, v)| (k.clone(), json_from_pb(v))).collect()
}

fn outcome(field: &str, o: buffa::EnumValue<pb::EffectOutcome>) -> Result<Outcome> {
    match o.as_known() {
        Some(pb::EffectOutcome::EFFECT_OUTCOME_SUCCEEDED) => Ok(Outcome::Succeeded),
        Some(pb::EffectOutcome::EFFECT_OUTCOME_FAILED) => Ok(Outcome::Failed),
        Some(pb::EffectOutcome::EFFECT_OUTCOME_SKIPPED) => Ok(Outcome::Skipped),
        Some(pb::EffectOutcome::EFFECT_OUTCOME_ROLLED_BACK) => Ok(Outcome::RolledBack),
        _ => Err(invalid(field, "a node reports succeeded, failed, skipped, or rolled back")),
    }
}

fn error_map(e: &pb::Error) -> Map<String, Value> {
    let mut m = json_map(&e.detail);
    m.insert("reason".to_owned(), Value::String(e.reason.clone()));
    m.insert("message".to_owned(), Value::String(e.message.clone()));
    m
}

fn effect(field: &str, e: &pb::Effect) -> Result<NodeEffect> {
    let optional = |kind: IdKind, name: &str, v: Option<&String>| {
        v.map(|s| id(kind, &format!("{field}.{name}"), s)).transpose()
    };
    Ok(NodeEffect {
        workload: optional(IdKind::Workload, "workload", e.workload.as_ref())?,
        plan: optional(IdKind::Plan, "plan", e.plan.as_ref())?,
        step: e.step,
        action: e.action.clone(),
        outcome: outcome(&format!("{field}.outcome"), e.outcome)?,
        error: e.error.as_option().map(error_map),
        after: (!e.after.is_empty()).then(|| json_map(&e.after)),
        actor: e
            .actor
            .as_option()
            .filter(|s| s.kind.is_some())
            .map(|s| crate::principal::from_pb(&format!("{field}.actor"), s))
            .transpose()?,
        grant: optional(IdKind::Grant, "grant", e.grant.as_ref())?,
    })
}

fn gap(field: &str, seq: u64, g: &agent::JournalGap) -> Result<Body> {
    if g.first_seq > g.last_seq || g.last_seq != seq {
        return Err(invalid(field, "a gap's sequence number is the last it stands for"));
    }
    Ok(Body::Gap { first: g.first_seq, last: g.last_seq, cause: g.cause.clone() })
}

fn segment(field: &str, e: &agent::StreamSegmentEvent) -> Result<Body> {
    let s =
        e.segment.as_option().ok_or_else(|| invalid(&format!("{field}.segment"), "required"))?;
    Ok(Body::Segment {
        workload: id(IdKind::Workload, &format!("{field}.workload"), &e.workload)?,
        run: (!e.run.is_empty())
            .then(|| id(IdKind::Run, &format!("{field}.run"), &e.run))
            .transpose()?,
        kind: crate::stream::kind_from_pb(&format!("{field}.kind"), e.kind)?,
        event: crate::stream::event_from_pb(&format!("{field}.event"), e.event)?,
        segment: crate::stream::segment_from_pb(&format!("{field}.segment"), s)?,
    })
}

fn body(field: &str, seq: u64, b: Option<&B>) -> Result<Body> {
    Ok(match b.ok_or_else(|| invalid(field, "required"))? {
        B::Effect(e) => Body::Effect(effect(&format!("{field}.effect"), e)?),
        B::Metering(_) => Body::Metering,
        B::BreakGlass(g) => Body::BreakGlass {
            operator: g.operator.clone(),
            reason: g.reason.clone(),
            session: g.session.clone(),
        },
        B::LeaseReleased(l) => Body::LeaseReleased {
            workload: id(
                IdKind::Workload,
                &format!("{field}.lease_released.workload"),
                &l.workload,
            )?,
            epoch: l.epoch,
        },
        B::Gap(g) => gap(&format!("{field}.gap"), seq, g)?,
        B::StreamSegment(e) => segment(&format!("{field}.stream_segment"), e)?,
        B::Volume(v) => {
            Body::Volume(crate::volume::reported_from_pb(&format!("{field}.volume"), v)?)
        }
        B::Snapshot(s) => {
            Body::Snapshot(crate::volume::snapshot_from_pb(&format!("{field}.snapshot"), s)?)
        }
        B::Backup(b) => {
            Body::Backup(crate::repository::reported_backup_from_pb(&format!("{field}.backup"), b)?)
        }
    })
}

/// The records a node sent, each translated or kept as malformed.
#[must_use]
pub fn records_from_pb(records: &[agent::JournalRecord]) -> Vec<Record> {
    records
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let field = format!("journal.records[{i}]");
            // The node's clock is informational: a time that does not translate is dropped.
            let occurred_at = r
                .occurred_at
                .as_option()
                .and_then(|t| time(&format!("{field}.occurred_at"), t).ok());
            let body = body(&field, r.seq, r.body.as_ref())
                .unwrap_or_else(|e| Body::Malformed { reason: e.to_string() });
            Record { seq: r.seq, occurred_at, body, count: r.count.max(1) }
        })
        .collect()
}
