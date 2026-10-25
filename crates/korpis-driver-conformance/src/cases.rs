// SPDX-License-Identifier: Apache-2.0

//! The cases, by what they test. Each is a function of the suite that ends in a verdict; one that
//! cannot get as far as its question fails, saying where it stopped.

use crate::case::{Report, Verdict};
use crate::suite::Suite;

mod cores;
mod ending;
mod exec;
mod limits;
mod objects;
mod prepare;
mod recover;

/// What §8 of docs/04-runtimes.md requires and no case tests yet: isolation's cases are held
/// for the maintainer (§4 of docs/20-roadmap.md), so a run says they were not run.
pub const UNTESTED: [(&str, &str); 1] =
    [("isolation: tier-appropriate escape attempts fail", "04 §8")];

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

/// `Recover` after the kills §8 of docs/04-runtimes.md names: the driver's, the kernel's for
/// memory, and a restart of the host.
pub async fn recover(s: &Suite, report: &mut Report) {
    let adopted = verdict(recover::adopts_after_a_kill(s).await);
    report.add(("recover_adopts_every_object_after_the_driver_is_killed", "04 §4.2"), adopted);
    let oom = verdict(recover::says_its_memory_ended_it(s).await);
    report.add(("an_object_killed_for_its_memory_says_so_and_is_adopted_stopped", "04 §8"), oom);
    let host = verdict(recover::a_restarted_host_runs_none(s).await);
    report.add(("after_the_host_restarts_nothing_runs_and_each_starts_again", "04 §8"), host);
}

/// Every limit the driver declares enforced, met and found held; and a capability declared that
/// the protocol can exercise.
pub async fn limits(s: &Suite, report: &mut Report) {
    report
        .add(("the_memory_limit_it_declares_is_held", "04 §4.1"), verdict(limits::memory(s).await));
    report
        .add(("the_process_limit_it_declares_is_held", "04 §4.1"), verdict(limits::pids(s).await));
    report.add(("the_disk_limit_it_declares_is_held", "04 §4.1"), verdict(limits::disk(s).await));
    report
        .add(("the_processor_limit_it_declares_is_held", "04 §4.1"), verdict(limits::cpu(s).await));
    report.add(
        ("a_resource_it_declares_resized_live_can_be_asked_for", "04 §4.2"),
        limits::hot_resize(s),
    );
}

/// `Exec` as it says it runs a command: as the object's own process, within its timeout and its
/// output limit; and refused by a driver that does not declare it.
pub async fn exec(s: &Suite, report: &mut Report) {
    let runs = verdict(exec::as_the_entry_runs(s).await);
    report.add(("a_command_runs_as_the_entry_runs", "04 §4.2"), runs);
    let timed = verdict(exec::killed_at_its_timeout(s).await);
    report.add(("a_command_is_killed_at_its_timeout_and_says_so", "04 §4.2"), timed);
    let limited = verdict(exec::sends_no_more_than_its_limit(s).await);
    report.add(("a_command_sends_no_more_than_its_limit_and_says_so", "04 §4.2"), limited);
    let refused = verdict(exec::undeclared_is_refused(s).await);
    report.add(("exec_it_does_not_declare_is_refused", "04 §4.1"), refused);
}

/// Dedicated cores, where the driver declares them held: an object given a core runs on its CPUs
/// alone, and the core of CPU 0 is refused.
pub async fn cores(s: &Suite, report: &mut Report) {
    let held = verdict(cores::held(s).await);
    report.add(("an_object_given_a_core_runs_on_its_cpus_alone", "04 §4.2"), held);
    let refused = verdict(cores::refuses_cpu_0s(s).await);
    report.add(("create_refuses_the_core_of_cpu_0", "04 §4.2"), refused);
}
