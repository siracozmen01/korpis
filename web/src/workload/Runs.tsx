// SPDX-License-Identifier: AGPL-3.0-or-later

// A task or scheduled workload's runs (§4 of docs/05-scheduling.md, §2 of
// docs/11-surface-web.md): each with what made it, where it is, when it was due and started, and
// how it ended; its console, which each run writes its own of (§4 of docs/14-streams.md), opened
// by naming it; and an active one cancelled by whoever may stop the workload. Read again whenever
// the workload's status says its newest run changed.

import { timestampDate } from "@bufbuild/protobuf/wkt";
import type { Timestamp } from "@bufbuild/protobuf/wkt";
import { useCallback, useState } from "react";
import { Button } from "react-aria-components";
import { useReach } from "../clients";
import { Console } from "../console/Console";
import { type Run, RunStatus } from "../gen/korpis/v1/workload_pb";
import { intl, t } from "../i18n/intl";
import { useLoad } from "../views/load";
import { refusal } from "../views/refusal";
import { CAUSE, STATUS, TRIGGER } from "./runWords";

const when = (at: Timestamp | undefined) =>
  at === undefined
    ? ""
    : intl.formatDate(timestampDate(at), { dateStyle: "short", timeStyle: "medium" });

/** How `r` ended, with its exit status when it has one, and how many occurrences it stands for
 * when a missed run records more than one. */
function ended(r: Run): string {
  const cause = CAUSE[r.cause];
  if (r.occurrences > 1) {
    return t(
      {
        defaultMessage: "{cause}, {count, plural, one {# occurrence} other {# occurrences}}",
        id: "x+99iEjx",
      },
      { cause, count: r.occurrences },
    );
  }
  if (r.exitStatus === undefined) return cause;
  return t(
    { defaultMessage: "{cause}, status {code}", id: "TTrkJX0h" },
    { cause, code: r.exitStatus },
  );
}

const active = (r: Run) => r.status === RunStatus.PENDING || r.status === RunStatus.RUNNING;

/** The runs of workload `id`; `newest` is its newest run as its status says, read again when it
 * changes. */
export function Runs(props: { readonly id: string; readonly newest: string }) {
  const { clients, allowed } = useReach();
  const may = (action: string) => allowed === undefined || allowed.has(action);
  const { id, newest } = props;
  const [error, setError] = useState<string>();
  const [open, setOpen] = useState<string>();
  // Read again when the status names another newest run, and after a cancellation.
  const [cancelled, setCancelled] = useState(0);
  const load = useCallback(async () => {
    const { runs } = await clients.runs.listRuns({ workload: id });
    return { runs, readFor: { newest, cancelled } };
  }, [clients, id, newest, cancelled]);
  const loaded = useLoad(load);
  const cancel = async (run: string) => {
    try {
      await clients.runs.cancelRun({ mutation: { idempotencyKey: crypto.randomUUID() }, run });
      setError(undefined);
      setCancelled((n) => n + 1);
    } catch (e) {
      setError(refusal(e));
    }
  };
  if (loaded.state === "hidden") return null;
  const runs = loaded.state === "done" ? loaded.value.runs : undefined;
  return (
    <section aria-labelledby="runs">
      <h2 id="runs">
        {t({ defaultMessage: "Runs", id: "Lmq3qotl", description: "a workload's runs" })}
      </h2>
      {error !== undefined && <p role="alert">{error}</p>}
      {loaded.state === "failed" && <p role="alert">{refusal(loaded.error)}</p>}
      {runs === undefined ? (
        <p role="status">{t({ defaultMessage: "Reading the runs…", id: "I8T1oMgK" })}</p>
      ) : runs.length === 0 ? (
        <p>{t({ defaultMessage: "No run yet.", id: "RYkMKXPj" })}</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>
                {t({ defaultMessage: "Made by", id: "0hHdDnYS", description: "a run's column" })}
              </th>
              <th>
                {t({ defaultMessage: "Status", id: "UMB/jFLh", description: "a run's column" })}
              </th>
              <th>
                {t({ defaultMessage: "Ended", id: "ReUrntUi", description: "a run's column" })}
              </th>
              <th>{t({ defaultMessage: "Due", id: "QPyR2U3r", description: "a run's column" })}</th>
              <th>
                {t({ defaultMessage: "Started", id: "NcvMwAcJ", description: "a run's column" })}
              </th>
              <th />
            </tr>
          </thead>
          <tbody>
            {runs.map((r) => (
              <tr key={r.id}>
                <td>
                  {r.attempt > 1
                    ? t(
                        { defaultMessage: "{trigger}, attempt {attempt}", id: "gmwG2Vqq" },
                        { trigger: TRIGGER[r.trigger], attempt: r.attempt },
                      )
                    : TRIGGER[r.trigger]}
                </td>
                <td>{STATUS[r.status]}</td>
                <td>{ended(r)}</td>
                <td>{when(r.scheduledFor)}</td>
                <td>{when(r.startedAt)}</td>
                <td>
                  {may("workload.console.read") && r.startedAt !== undefined && (
                    <Button
                      aria-pressed={open === r.id}
                      onPress={() => {
                        setOpen(open === r.id ? undefined : r.id);
                      }}
                    >
                      {t({
                        defaultMessage: "Console",
                        id: "88JBbyvf",
                        description: "a run's console",
                      })}
                    </Button>
                  )}
                  {may("workload.stop") && active(r) && (
                    <Button
                      onPress={() => {
                        void cancel(r.id);
                      }}
                    >
                      {t({ defaultMessage: "Cancel the run", id: "XdEmcxs6" })}
                    </Button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {open !== undefined && <Console key={open} id={id} run={open} typing={false} />}
    </section>
  );
}
