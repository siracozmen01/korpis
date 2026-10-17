// SPDX-License-Identifier: AGPL-3.0-or-later

// A workload's screen (§2 and §3 of docs/11-surface-web.md), live over its watch: what it is
// converging to and how far it has come, the stage it is at when it has not converged, and what
// its node observed, each value known, stale, or unknown; and its console and files (§8), which
// a share link's screen opens only when asked, since each is a use of the link.

import { type ReactNode, useState } from "react";
import { Button } from "react-aria-components";
import { useReach } from "../clients";
import { Console } from "../console/Console";
import { FileManager } from "../files/FileManager";
import { READ, WRITE } from "../files/session";
import { Actions } from "../workload/Actions";
import { Grants } from "../workload/Grants";
import { Share } from "../workload/Share";
import { diagnosis } from "../workload/diagnosis";
import { age, freshness } from "../workload/freshness";
import { Observed } from "../workload/Observed";
import { TimelineView } from "../workload/TimelineView";
import { useNow } from "../workload/useNow";
import { PowerState } from "../gen/korpis/v1/workload_pb";
import { CONVERGENCE, DECLARED } from "../workload/words";
import { refusal } from "./refusal";
import { useLive } from "./watch";
import { t } from "../i18n/intl";
import { Configuration } from "../config/Configuration";

/** The workload `id`. */
export function WorkloadPage({ id }: { readonly id: string }) {
  const { clients, metered, allowed } = useReach();
  const may = (action: string) => allowed === undefined || allowed.has(action);
  const live = useLive(clients, id, !metered);
  const now = useNow();
  const { status, ended } = live;
  if (live.hidden) return null;
  if (status === undefined) {
    return ended === undefined ? (
      <p role="status">{t({ defaultMessage: "Reading the workload\u2026", id: "FId+XQnj" })}</p>
    ) : (
      <main>
        <p role="alert">{refusal(ended)}</p>
        {metered && <Reopen reopen={live.reopen} />}
      </main>
    );
  }
  const said = diagnosis(status, now);
  return (
    <main>
      <h1>{status.workload?.name ?? id}</h1>
      <p>
        {t(
          { defaultMessage: "Declared {declared}, {convergence}", id: "8RMoim+k" },
          {
            declared: DECLARED[status.workload?.power?.desired ?? PowerState.UNSPECIFIED],
            convergence: CONVERGENCE[status.convergence],
          },
        )}
      </p>
      {said !== undefined && <p role="status">{said}</p>}
      {ended !== undefined && (
        <div role="alert">
          <p>
            {t(
              {
                defaultMessage:
                  "Not live: last heard {ago} ago.{reconnecting, select, yes { Reconnecting…} other {}}",
                id: "xIksTt1G",
              },
              { ago: age(now - live.heard.at), reconnecting: metered ? "no" : "yes" },
            )}
          </p>
          {metered && <Reopen reopen={live.reopen} />}
        </div>
      )}
      <Actions status={status} />
      {live.timeline !== undefined && <TimelineView timeline={live.timeline} />}
      <Observed status={status} freshness={freshness(status, live.heard)} now={now} />
      {may("workload.console.read") && (
        <Asked metered={metered} what={t({ defaultMessage: "Open the console", id: "cLNad6D5" })}>
          <Console id={id} typing={may("workload.console.write")} />
        </Asked>
      )}
      {(may(READ) || may(WRITE)) && (
        <Asked metered={metered} what={t({ defaultMessage: "Open the files", id: "KTRrirwq" })}>
          <FileManager id={id} writes={may(WRITE)} />
        </Asked>
      )}
      {!metered && (
        <>
          {status.head !== undefined && <Configuration id={id} head={status.head} />}
          <Grants id={id} />
          <Share id={id} />
        </>
      )}
    </main>
  );
}

/** What opens a session of its own, at once on a signed-in screen and only when asked on a share
 * link's, where each opening spends a use: the screen's own opening is the one use the link's
 * holder was told it costs (§6 of docs/11-surface-web.md). */
function Asked(props: {
  readonly metered: boolean;
  readonly what: string;
  readonly children: ReactNode;
}) {
  const [asked, setAsked] = useState(!props.metered);
  if (asked) return props.children;
  return (
    <p>
      <Button
        onPress={() => {
          setAsked(true);
        }}
      >
        {t(
          { defaultMessage: "{what} (uses one of the link's uses)", id: "zHB1mRzN" },
          { what: props.what },
        )}
      </Button>
    </p>
  );
}

/** Opening the live view again through a share link, which spends one of its uses. */
function Reopen({ reopen }: { readonly reopen: () => void }) {
  return (
    <Button onPress={reopen}>
      {t({ defaultMessage: "Open it again (uses one of the link's uses)", id: "acNLq/ZK" })}
    </Button>
  );
}
