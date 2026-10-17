// SPDX-License-Identifier: AGPL-3.0-or-later

// A file session kept open while the file manager is shown: opened when it mounts, closed when it
// unmounts, and opened again after it is lost, by itself for a signed-in user and on request
// through a share link, since each opening spends one of the link's uses.

import { useCallback, useEffect, useState } from "react";
import type { Clients } from "../clients";
import type { Relay } from "../console/relay";
import { refusal } from "../views/refusal";
import { FileSession } from "./session";

/** How long a lost session waits before it is opened again. */
const AGAIN_MS = 3_000;

/** What the file manager holds of its session. */
export interface Held {
  /** The session, while it is open. */
  readonly fs: FileSession | undefined;
  /** Why it is not, once it was lost or refused. */
  readonly ended: string | undefined;
  /** Opens it again. */
  readonly reopen: () => void;
  /** Says the session in use was lost, so it is opened again. */
  readonly lost: (why: string) => void;
}

/** The file session of workload `workload`, writing when `writes`. */
export function useFiles(
  opening: {
    readonly clients: Clients;
    readonly relay: () => Promise<Relay>;
    readonly workload: string;
  },
  writes: boolean,
  automatic: boolean,
): Held {
  const [fs, setFs] = useState<FileSession>();
  const [ended, setEnded] = useState<string>();
  const [attempt, setAttempt] = useState(0);
  const { clients, relay, workload } = opening;
  useEffect(() => {
    let current: FileSession | undefined;
    let gone = false;
    relay()
      .then((r) => FileSession.open({ clients, relay: r, workload }, writes))
      .then(
        (s) => {
          if (gone) {
            s.close();
            return;
          }
          current = s;
          setFs(s);
          setEnded(undefined);
        },
        (e: unknown) => {
          if (!gone) setEnded(refusal(e));
        },
      );
    return () => {
      gone = true;
      current?.close();
      setFs(undefined);
    };
  }, [clients, relay, workload, writes, attempt]);
  const reopen = useCallback(() => {
    setAttempt((n) => n + 1);
  }, []);
  const lost = useCallback(
    (why: string) => {
      setFs(undefined);
      setEnded(why);
      if (automatic) setTimeout(reopen, AGAIN_MS);
    },
    [automatic, reopen],
  );
  return { fs, ended, reopen, lost };
}
