// SPDX-License-Identifier: Apache-2.0

//! The cases, by what they test. Each is a function of the suite that ends in a verdict; one that
//! cannot get as far as its question fails, saying where it stopped.

use crate::case::{Report, Verdict};
use crate::suite::Suite;

mod ending;
mod objects;
mod prepare;

/// What a case that stopped short says.
type Stopped = String;

/// The verdict of a case that may have stopped short.
fn verdict(r: Result<Verdict, Stopped>) -> Verdict {
    r.unwrap_or_else(Verdict::Fail)
}

/// The lifecycle of §4.2 of docs/04-runtimes.md: every method, its failures, and each asked twice.
pub async fn lifecycle(s: &Suite, report: &mut Report) {
    report.add(("describes_itself", "04 §4.1"), prepare::describes_itself(s));
    report.add(("prepare_changes_nothing", "04 §4.2"), verdict(prepare::pure(s).await));
    let refused = verdict(prepare::refuses_another_tier(s).await);
    report.add(("prepare_refuses_a_tier_it_does_not_declare", "04 §4.2"), refused);
    let refused = verdict(prepare::refuses_another_driver(s).await);
    report.add(("prepare_refuses_another_driver", "04 §4.2"), refused);
    report.add(("create_runs_nothing", "04 §4.2"), verdict(objects::create_runs_nothing(s).await));
    let again = verdict(objects::create_again_is_the_same(s).await);
    report.add(("create_again_is_the_same_object", "04 §4.2"), again);
    let outside = verdict(objects::refuses_a_volume_outside(s).await);
    report.add(("create_refuses_a_volume_outside_the_root", "02 §6.1"), outside);
    let started = verdict(objects::start_runs_it(s).await);
    report.add(("start_runs_it_and_again_changes_nothing", "04 §4.2"), started);
    let unknown = verdict(objects::unknown_is_not_found(s).await);
    report.add(("an_object_it_does_not_hold_is_not_found", "04 §4.2"), unknown);
    let stopped = verdict(ending::stop_reaches_the_entry(s).await);
    report.add(("stop_reaches_the_entry_and_again_changes_nothing", "04 §4.2"), stopped);
    let killed = verdict(ending::stop_kills_after_grace(s).await);
    report.add(("stop_kills_what_outlives_its_grace", "04 §4.2"), killed);
    report
        .add(("kill_ends_it_and_again_changes_nothing", "04 §4.2"), verdict(ending::kill(s).await));
    let exited = verdict(ending::exit_is_reported(s).await);
    report.add(("an_exit_is_inspected_with_its_status_and_sent_as_an_event", "04 §4.2"), exited);
    let destroyed = verdict(ending::destroy_releases(s).await);
    report.add(("destroy_releases_it_and_again_is_not_found", "04 §4.2"), destroyed);
}
