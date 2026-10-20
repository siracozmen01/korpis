// SPDX-License-Identifier: AGPL-3.0-or-later

// A request the network failed, sent again (§9 of docs/11-surface-web.md). A phone that moves
// from one network to another, or a machine whose interfaces change, has the browser abandon
// every request in flight (Chromium's `ERR_NETWORK_CHANGED`), and a person is left with "Failed
// to fetch" on every screen that was loading. So a call the network failed before any answer
// came is sent again, twice, after a quarter of a second and after a second. Sending it again is
// safe where the panel uses it: a read changes nothing, a mutation carries its idempotency key so
// the control plane answers a repeat with the first answer, a ceremony without one spends a
// single-use proof so a repeat is refused rather than applied twice (§6 of docs/10-api.md), and a
// relay session's frames are numbered so a repeated one is never applied twice. It is not used
// for a share link's calls, each of which spends a use: one whose answer was lost may still have
// spent it, and a link's screen calls nothing again without being asked.
//
// A ceremony sent again after a failure is refused when the first attempt arrived and only its
// answer was lost, so the refusal is not the whole truth: its proof was spent by an attempt that
// may have done what it was asked. Such an answer carries `LOST_BEFORE`, which a ceremony's
// screen reads (`lostAnswer` in views/refusal.ts) to say what may have happened.

/** How long to wait before each further attempt. */
const WAITS: readonly number[] = [250, 1000];

/** The header an answer is given when an earlier attempt of its call had none. */
export const LOST_BEFORE = "korpis-earlier-answer-lost";

/** `answer`, marked as following an attempt whose answer was lost, unless it succeeded. */
function marked(answer: Response): Response {
  if (answer.ok) return answer;
  const headers = new Headers(answer.headers);
  headers.set(LOST_BEFORE, "1");
  const { status, statusText } = answer;
  return new Response(answer.body, { status, statusText, headers });
}

/** Waits `ms`, or until `signal` aborts. */
function pause(ms: number, signal: AbortSignal | null | undefined): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted === true) {
      reject(signal.reason as Error);
      return;
    }
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", stop);
      resolve();
    }, ms);
    const stop = () => {
      clearTimeout(timer);
      reject(signal?.reason as Error);
    };
    signal?.addEventListener("abort", stop, { once: true });
  });
}

/** `inner`, sending a request again when the network failed it with no answer: `fetch` rejects
 * with a `TypeError` then, and with anything else (an abort) only when asked to stop. A request
 * given as a `Request` holds a body that cannot be read twice, so it is sent once. A refusal
 * that follows a failure is marked with `LOST_BEFORE`. */
export function retrying(inner: typeof fetch, waits: readonly number[] = WAITS): typeof fetch {
  return async (input, init) => {
    if (input instanceof Request) return inner(input, init);
    let failed = false;
    for (const wait of waits) {
      try {
        const answer = await inner(input, init);
        return failed ? marked(answer) : answer;
      } catch (e) {
        if (!(e instanceof TypeError) || init?.signal?.aborted === true) throw e;
        failed = true;
        await pause(wait, init?.signal);
      }
    }
    return marked(await inner(input, init));
  };
}
