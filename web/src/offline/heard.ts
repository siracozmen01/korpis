// SPDX-License-Identifier: AGPL-3.0-or-later

// When this browser last heard from the control plane: a time, and nothing the control plane
// said, kept so a panel loaded while it cannot be reached can say how long that has been (question
// 3 of §10 of docs/11-surface-web.md). Storage the browser refuses leaves it unknown.

const KEY = "korpis.heard";

/** Records that the control plane answered at `at`. */
export function heard(at: number = Date.now()): void {
  try {
    localStorage.setItem(KEY, String(at));
  } catch {
    // Refused storage: the time is not known later, which the page then says.
  }
}

/** When the control plane last answered this browser, if it is known. */
export function lastHeard(): number | undefined {
  try {
    const at = Number(localStorage.getItem(KEY));
    return Number.isFinite(at) && at > 0 ? at : undefined;
  } catch {
    return undefined;
  }
}
