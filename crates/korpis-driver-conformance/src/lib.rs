// SPDX-License-Identifier: Apache-2.0

//! The driver conformance suite (§8 of docs/04-runtimes.md): an executable definition of what a
//! runtime driver must do, tested through the driver protocol (`korpis.driver.v1`) alone, so a
//! driver written in any language is tested as a node would use it. The suite stands in for the
//! node: it is given the driver's socket and the node's paths the driver was started with
//! ([`Fixture`]), lays the image of its probe into the content store, makes each volume as the
//! helper would, and fills in what the agent fills in before `Create`. Each case ends in a
//! [`Verdict`], and a driver conforms when none fails.
//!
//! It is part of the separately published Apache-2.0 artifact (§11 of docs/10-api.md), and
//! depends on nothing of Korpis's but the protocol's definitions.

pub mod case;
mod cases;
pub mod client;
pub mod fixture;
pub mod image;
pub mod probe;
pub mod process;
pub mod suite;
pub mod volume;

pub use case::{Outcome, Report, Verdict};
pub use fixture::{Fixture, Launch};

/// Runs every case against the driver `fixture` names, then destroys what the run made.
///
/// # Errors
/// The suite could not start: the driver did not describe itself, or the probe's image could not
/// be laid into its content store.
pub async fn run(fixture: Fixture) -> Result<Report, String> {
    let suite = suite::Suite::new(fixture).await?;
    let mut report = Report { driver: suite.info.name.clone(), ..Report::default() };
    // Each group of cases is a large future, held on the heap rather than the caller's stack.
    Box::pin(cases::lifecycle(&suite, &mut report)).await;
    Box::pin(cases::recover(&suite, &mut report)).await;
    Box::pin(cases::limits(&suite, &mut report)).await;
    Box::pin(cases::exec(&suite, &mut report)).await;
    report.untested.extend(cases::UNTESTED);
    suite.clean().await;
    suite.end();
    Ok(report)
}
