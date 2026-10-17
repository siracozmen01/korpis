// SPDX-License-Identifier: AGPL-3.0-or-later

// Reading for a screen: what is being read, what was read, or why it could not be. A refusal
// that means "not yours to see" hides what was asked for, since an object the caller cannot read
// is not shown as locked (§5 of docs/11-surface-web.md); any other failure is said, because a
// screen that shows nothing when the control plane is down would be lying (§2).

import { Code, ConnectError } from "@connectrpc/connect";
import { useEffect, useState } from "react";

/** A read in progress, done, or failed. */
export type Loaded<T> =
  | { readonly state: "loading" }
  | { readonly state: "done"; readonly value: T }
  | { readonly state: "hidden" }
  | { readonly state: "failed"; readonly error: unknown };

/** Whether a refusal means the caller may not see the object, so it is left out. */
export function notTheirs(e: unknown): boolean {
  const code = ConnectError.from(e).code;
  return code === Code.NotFound || code === Code.PermissionDenied;
}

/** Reads with `load` whenever it changes; callers keep it stable with `useCallback`. */
export function useLoad<T>(load: () => Promise<T>): Loaded<T> {
  const [result, setResult] = useState<{
    readonly of: () => Promise<T>;
    readonly loaded: Loaded<T>;
  }>();
  useEffect(() => {
    let current = true;
    const settle = (loaded: Loaded<T>) => {
      if (current) setResult({ of: load, loaded });
    };
    load().then(
      (value) => {
        settle({ state: "done", value });
      },
      (error: unknown) => {
        settle(notTheirs(error) ? { state: "hidden" } : { state: "failed", error });
      },
    );
    return () => {
      current = false;
    };
  }, [load]);
  return result?.of === load ? result.loaded : { state: "loading" };
}
