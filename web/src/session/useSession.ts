// SPDX-License-Identifier: AGPL-3.0-or-later

// The session as a component sees it: signed in, signed out, not yet known, or not knowable
// because the control plane cannot be reached, rendered again whenever any tab changes it.

import { useEffect, useState, useSyncExternalStore } from "react";
import type { Session } from "./session";

/** What is known of the session. */
export type Standing = "unknown" | "signed-in" | "signed-out" | "unreachable";

/** The standing of `session`, learnt by exchanging the cookie once if nothing is known yet, and a
 * way to ask again after the control plane could not be reached. */
export function useSession(session: Session): { standing: Standing; retry: () => void } {
  const [unreachable, setUnreachable] = useState(false);
  const known = useSyncExternalStore(
    (watch) => session.subscribe(watch),
    (): Standing =>
      session.usable() ? "signed-in" : session.signedOut() ? "signed-out" : "unknown",
  );
  useEffect(() => {
    if (known !== "unknown" || unreachable) return;
    session.token().catch(() => {
      setUnreachable(true);
    });
  }, [session, known, unreachable]);
  const retry = () => {
    setUnreachable(false);
  };
  return { standing: unreachable && known === "unknown" ? "unreachable" : known, retry };
}
