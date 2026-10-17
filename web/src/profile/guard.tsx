// SPDX-License-Identifier: AGPL-3.0-or-later

// A change that needs a recent sign-in (§9.4 of docs/08-identity.md): tried as it is, and when the
// control plane answers `session.not_recent`, the screen asks the user to confirm it is them and
// then tries the same change again, so a change is never lost to the ten-minute rule.

import { type ReactNode, useState } from "react";
import { Reauthenticate } from "../views/Reauthenticate";
import { reasonOf, refusal } from "../views/refusal";
import type { ProfileServices } from "./services";

/** A way to run a guarded change, what it said, and the re-authentication it may be waiting on. */
export interface Guard {
  readonly run: (change: () => Promise<void>) => void;
  readonly said: ReactNode;
}

/** Guards changes made through `services`. */
export function useGuard(services: ProfileServices): Guard {
  const [waiting, setWaiting] = useState<() => Promise<void>>();
  const [error, setError] = useState<string>();
  const run = (change: () => Promise<void>) => {
    setError(undefined);
    change().catch((e: unknown) => {
      if (reasonOf(e) === "session.not_recent") setWaiting(() => change);
      else setError(refusal(e));
    });
  };
  const said =
    waiting !== undefined ? (
      <Reauthenticate
        accounts={services.accounts}
        profiles={services.profiles}
        assert={services.assert}
        done={() => {
          setWaiting(undefined);
          run(waiting);
        }}
      />
    ) : (
      error !== undefined && <p role="alert">{error}</p>
    );
  return { run, said };
}
