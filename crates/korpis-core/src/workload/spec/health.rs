// SPDX-License-Identifier: AGPL-3.0-or-later

//! Health probes: how readiness and liveness are judged.

use super::SpecError;
use super::endpoints::EndpointSpec;
use std::time::Duration;

/// What a probe checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeKind {
    /// A TCP connection to an endpoint succeeds.
    Tcp {
        /// The endpoint, by name.
        endpoint: String,
    },
    /// An HTTP request to an endpoint returns the expected status.
    Http {
        /// The endpoint, by name.
        endpoint: String,
        /// The request path.
        path: String,
        /// The status expected; zero for any 2xx.
        expect_status: u16,
    },
    /// A protocol query provided by an extension, e.g. `minecraft`.
    Query {
        /// The query protocol.
        protocol: String,
        /// The endpoint, by name.
        endpoint: String,
    },
    /// A command inside the workload exits zero.
    Exec {
        /// The command and its arguments.
        command: Vec<String>,
    },
    /// The guest agent of a virtual machine reports healthy.
    GuestAgent,
    /// The workload exits with one of these codes (tasks).
    Exit {
        /// The codes that mean success; empty for zero only.
        success_codes: Vec<i32>,
    },
    /// The workload reports in at least this often.
    Heartbeat {
        /// The longest silence that is still healthy.
        max_interval: Duration,
    },
}

/// One probe and its timing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// What it checks.
    pub kind: ProbeKind,
    /// How often.
    pub period: Option<Duration>,
    /// How long one attempt may take.
    pub timeout: Option<Duration>,
    /// Consecutive failures before the state changes; zero for the default.
    pub failures: u32,
    /// How long after start before probing begins.
    pub initial_delay: Option<Duration>,
}

/// Readiness and liveness.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HealthSpec {
    /// When it can take traffic.
    pub readiness: Option<Probe>,
    /// When it should be restarted.
    pub liveness: Option<Probe>,
}

fn check_probe(at: &str, p: &Probe, endpoints: &[EndpointSpec]) -> Result<(), SpecError> {
    let names = |e: &str| endpoints.iter().any(|x| x.name == e);
    match &p.kind {
        ProbeKind::Tcp { endpoint }
        | ProbeKind::Http { endpoint, .. }
        | ProbeKind::Query { endpoint, .. }
            if !names(endpoint) =>
        {
            Err(SpecError::new(at, format!("no endpoint named {endpoint:?}")))
        }
        ProbeKind::Http { path, .. } if !path.starts_with('/') => {
            Err(SpecError::new(at, "an HTTP probe path starts with /"))
        }
        ProbeKind::Exec { command } if command.is_empty() => {
            Err(SpecError::new(at, "an exec probe needs a command"))
        }
        ProbeKind::Heartbeat { max_interval } if max_interval.is_zero() => {
            Err(SpecError::new(at, "a heartbeat needs an interval"))
        }
        _ => Ok(()),
    }?;
    match (p.period, p.timeout) {
        (Some(period), Some(timeout)) if timeout > period => {
            Err(SpecError::new(at, "the timeout exceeds the period"))
        }
        _ => Ok(()),
    }
}

impl HealthSpec {
    /// Checks each probe against the endpoints it names.
    pub fn validate(&self, endpoints: &[EndpointSpec]) -> Result<(), SpecError> {
        if let Some(p) = &self.readiness {
            check_probe("health.readiness", p, endpoints)?;
        }
        if let Some(p) = &self.liveness {
            check_probe("health.liveness", p, endpoints)?;
        }
        Ok(())
    }
}
