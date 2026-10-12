// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `probes` block (Rule K-16).

use super::args::Args;
use super::scalars::duration;
use korpis_core::recipe::RecipeError;
use korpis_core::workload::spec::health::{HealthSpec, Probe, ProbeKind};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::time::Duration;

const TIMING: [&str; 4] = ["period", "timeout", "failures", "initial_delay"];

fn opt_duration(a: &Args<'_>, key: &str) -> Result<Option<Duration>, RecipeError> {
    a.opt_str(key)?.map(|s| duration(&format!("{}.{key}", a.at), &s)).transpose()
}

fn http(a: &Args<'_>) -> Result<ProbeKind, RecipeError> {
    let expect_status = a
        .opt_u32("expect_status")?
        .map_or(Ok(0), u16::try_from)
        .map_err(|_| a.err("expect_status", "a status code"))?;
    Ok(ProbeKind::Http {
        endpoint: a.str("endpoint")?,
        path: a.opt_str("path")?.unwrap_or_else(|| "/".into()),
        expect_status,
    })
}

fn success_codes(a: &Args<'_>) -> Result<Vec<i32>, RecipeError> {
    let codes = match a.raw("success_codes") {
        None => Vec::new(),
        Some(Value::Array(v)) => v
            .iter()
            .map(|c| {
                c.as_i64()
                    .and_then(|c| i32::try_from(c).ok())
                    .ok_or_else(|| a.err("success_codes", "exit codes"))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(a.err("success_codes", "a list of exit codes")),
    };
    Ok(codes)
}

fn kind(a: &Args<'_>) -> Result<ProbeKind, RecipeError> {
    let k = a.str("kind")?;
    let allow = |extra: &[&str]| {
        let mut keys = vec!["kind"];
        keys.extend(TIMING);
        keys.extend(extra);
        a.only(&keys)
    };
    Ok(match k.as_str() {
        "tcp" => {
            allow(&["endpoint"])?;
            ProbeKind::Tcp { endpoint: a.str("endpoint")? }
        }
        "http" => {
            allow(&["endpoint", "path", "expect_status"])?;
            http(a)?
        }
        "query" => {
            allow(&["protocol", "endpoint"])?;
            ProbeKind::Query { protocol: a.str("protocol")?, endpoint: a.str("endpoint")? }
        }
        "exec" => {
            allow(&["command"])?;
            ProbeKind::Exec { command: a.strings("command")? }
        }
        "guest_agent" => {
            allow(&[])?;
            ProbeKind::GuestAgent
        }
        "exit" => {
            allow(&["success_codes"])?;
            ProbeKind::Exit { success_codes: success_codes(a)? }
        }
        "heartbeat" => {
            allow(&["max_interval"])?;
            ProbeKind::Heartbeat {
                max_interval: opt_duration(a, "max_interval")?
                    .ok_or_else(|| a.err("max_interval", "required"))?,
            }
        }
        other => return Err(a.err("kind", format!("{other:?} is not a probe kind"))),
    })
}

fn probe(at: &str, m: &BTreeMap<String, Value>) -> Result<Probe, RecipeError> {
    let map: Map<String, Value> = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let a = Args::from_map(at.to_owned(), &map);
    Ok(Probe {
        kind: kind(&a)?,
        period: opt_duration(&a, "period")?,
        timeout: opt_duration(&a, "timeout")?,
        failures: a.opt_u32("failures")?.unwrap_or(0),
        initial_delay: opt_duration(&a, "initial_delay")?,
    })
}

/// Readiness and liveness.
pub fn probes(p: Option<&super::dto::Probes>) -> Result<HealthSpec, RecipeError> {
    let Some(p) = p else { return Ok(HealthSpec::default()) };
    Ok(HealthSpec {
        readiness: p.readiness.as_ref().map(|m| probe("probes.readiness", m)).transpose()?,
        liveness: p.liveness.as_ref().map(|m| probe("probes.liveness", m)).transpose()?,
    })
}
