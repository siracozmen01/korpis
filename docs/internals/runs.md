# Runs

A `scheduled` workload with a nightly cron produces a run every night. Runs are executions of one
workload, not workloads: modelling them as workloads would multiply objects by the number of nights
and break every quota, grant and name.

## Contents

- [The run](#the-run)
- [What makes a run](#what-makes-a-run)
- [Where a run runs, and what ends it](#where-a-run-runs-and-what-ends-it)
- [Concurrency and catching up](#concurrency-and-catching-up)
- [Power, cancellation and retention](#power-cancellation-and-retention)

## The run

```
Run
  workload, intent      the declaration that produced it
  trigger               schedule | manual | retry | event
  scheduled_for, started_at, ended_at
  node                  the node it ran on
  status                pending | running | succeeded | failed | cancelled | missed
  exit_status, attempt
  streams               its own output, so Tuesday's failure is readable after Wednesday succeeds
  cycle                 the power cycle that requested it, so a requested run is made exactly once
  occurrences           how many occurrences a missed run stands for, so a skipped week is one row
  cause                 exit | timeout | cancelled | stopped | replaced | concurrency |
                        not_started | skipped | node_lost

RunPolicy
  concurrency      allow | forbid | replace            default forbid
  retries, backoff                                     default none, 10 s
  timeout                                              default none
  keep_successful, keep_failed                         default 3, 10
  catch_up                                             default false
  max_active       under allow: 10 unless declared, at most 100; 1 under forbid and replace
```

`concurrency` and `catch_up` cause incidents when implicit. Two backups running against one volume
at once is how people lose data, and catching up six missed runs after an outage fires them all at
once. So the defaults can do neither: an implicit `forbid` at worst skips a night, recorded where
anyone reading runs sees it. There is no default timeout, because killing a legitimately long first
backup is worse than none; a hung run under `forbid` turns every later night into a missed run with
cause `concurrency`, which is how it is noticed.

## What makes a run

The control plane makes every run; a node executes only the runs its assignment names.

- **schedule.** For every `scheduled` workload whose power target is running, the control plane
  evaluates the target intent's schedule in its time zone and keeps the next occurrence. The store
  holds at most one scheduled run per workload and occurrence, so two replicas evaluating the same
  minute make one run.
- **manual.** A higher power cycle (a restart) on a task or scheduled workload requests one run,
  made once for that cycle. A task's first run comes this way: its creating plan brings its power to
  running at cycle zero. So a task runs once when created and once per restart, and restart means
  "do it again" for every lifecycle. For a scheduled workload cycle zero requests nothing.
- **retry.** A failed run with attempts left makes the next, the n-th retry waiting
  `backoff x 2^(n - 1)`. A failure the node reports and a lost node are retried alike.
- **event.** Reserved for a later trigger source; it stays in the frozen schema.

A run is made whether or not the workload is placed; it is pending until a node starts it.

**Time zones** are IANA names, UTC when unset, checked at declaration against the control plane's
zone database (the machine's, `/usr/share/zoneinfo` or `TZDIR`). A local time a daylight-saving
change skips runs at the first instant after the gap; one it repeats runs once, at its first
occurrence. Cron in UTC would run a 03:00 Berlin backup at 04:00 or 05:00 by season. The cost: a
zone whose rules changed after the database was built runs an hour off until the machine updates it.
Schedules are five fields read as Vixie cron reads them (`n/k` to the field's end, Sunday as 0 or 7,
a day matching either restricted day field is due), and one naming only days its months never reach
is refused. A stored intent writes its policy with defaults and `UTC` filled in, so later defaults
cannot change it; a policy on a workload that cannot use it is refused rather than ignored.

## Where a run runs, and what ends it

A run executes on the node its workload is placed on, as a container instance of its own named for
the run, with the workload's volumes, endpoints and identity. **The workload's reservation is held
while it is placed, not only while a run runs**, which guarantees the 03:00 run room on a node that
filled up during the day. An idle nightly job therefore holds its reservation all day; one that
would rather not declares a limit and no reservation, and its run takes what is free at that moment.

The workload's own object is never started; plan steps prepare its target, and runs start as the
assignment names them. A run's object is labelled with the run, started once and never revived when
it exits, and journaled as `run.started` and `run.ended` with exit status and cause. An ended object
is kept while the run is still named, so a node not yet told the run ended never runs it twice. A
restarted agent takes runs over by label and counts their timeouts from their real start. Concurrent
runs share the workload's network namespace, so a workload declaring endpoints with `allow` is
refused at declaration.

The control plane believes run reports only from a node the workload is assigned to, for a run no
other node started, and judges the exit status against the `exit` probe of the intent the run was
made under (zero alone when none is declared). The node says why it stopped a run, never how it
ended: `timeout` is failed, `cancelled` cancelled, anything else judged by exit status. A report the
run has moved past changes nothing. Node clock readings are kept between when the run could have
happened and when the report arrived. The node enforces the timeout itself (signal, grace, kill), so
it holds while the control plane is unreachable. When the node no longer holds a current lease on
the workload, its runs are failed with cause `node_lost` within one pass, and retried where attempts
remain. A task or scheduled workload has converged once its power target is reached; its newest run
is shown on its status, since how a run was judged is the control plane's, not the node's.

## Concurrency and catching up

A run is active while pending or running. When one is due and another is active:

- `forbid`: the new run is recorded missed, cause `concurrency`.
- `replace`: the active run is cancelled, cause `replaced`, and the new one made.
- `allow`: both run, up to `max_active`; past it the new run is missed, cause `concurrency`.

Manual and retry runs follow the same rule, so a restart into a running `forbid` workload is
recorded, not lost. Under `forbid` and `replace` the assignment names only the running run or the
oldest pending one, so queued runs reach the node one at a time without the node needing a policy.

`max_active` bounds `allow` because every running run writes a console stream on its node, and a
disk quota shown to a tenant must hold what the workload can actually use. The quota counts one
stream per run retention keeps and per run that can be active (`keep_successful + keep_failed +
max_active`): with the defaults, fourteen streams of 72 MiB, 1008 MiB of the workload's disk quota.
A job needing less declares fewer kept runs and a smaller console.

An occurrence is on time when its run is made within a minute of it. Past occurrences missed while
the control plane was down become, with `catch_up: false`, one missed run (cause `skipped`, its
first occurrence, and how many), and the schedule continues from now. With `catch_up: true` each
becomes a run, oldest first, at most a hundred (older ones recorded as one missed run): under
`forbid` one after another, under `replace` only the newest, under `allow` up to `max_active`
together, which is the herd `allow` asks for.

A pending run that never starts (its node offline, or unplaced) stays active. At its next
occurrence, `catch_up: false` records it missed (`not_started`) and makes the new one, so a node
back after three nights runs tonight's backup, not the first night's; `catch_up: true` keeps it, and
the node runs all three in order. A node that started it while out of touch reports so on return,
and the run is recorded as it ran.

## Power, cancellation and retention

Power stopped suspends the schedule: no run is made, active runs are cancelled with cause `stopped`,
and nothing is missed meanwhile, since nobody wanted it to run. `CancelRun` cancels one run under
`workload.stop`. A retired workload's active run is cancelled as `stopped`. When a run ends, the
control plane keeps the newest `keep_successful` succeeded runs and the newest `keep_failed` of
every other ending, and deletes the rest with their streams; the node deletes a run's stream volume
once the assignment no longer lists it among the runs the control plane holds. Runs are read under
`workload.read`, their streams under `workload.console.read`.

Every ten seconds the control plane takes each workload with runs in a transaction holding it and
its run state, and decides from that, the time and the zone, as one function of what the store
holds, so it is tested at chosen moments. Each decision is an effect (`run.made`, `run.missed`,
`run.cancelled`), and each change announces the workload to its node and watchers. While a restore
settles the run maker makes and ends nothing.
