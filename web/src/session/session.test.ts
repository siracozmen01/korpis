// SPDX-License-Identifier: AGPL-3.0-or-later

import { type Channel, type Shared, MARGIN_MS, Session, pageLock as lock } from "./session";

/** Channels of two tabs that hear each other, never themselves. */
function tabs(): [Channel, Channel] {
  const hearers: [((m: Shared) => void)[], ((m: Shared) => void)[]] = [[], []];
  const channel = (mine: ((m: Shared) => void)[], theirs: ((m: Shared) => void)[]): Channel => ({
    post: (m) => {
      theirs.forEach((h) => {
        h(m);
      });
    },
    listen: (h) => {
      mine.push(h);
    },
  });
  return [channel(hearers[0], hearers[1]), channel(hearers[1], hearers[0])];
}

test("tabs exchange one at a time, and a tab that waited uses the token the other got", async () => {
  let now = 1_000_000;
  let exchanges = 0;
  const exchange = () => {
    exchanges += 1;
    return Promise.resolve({ token: `t${String(exchanges)}`, expiresAt: now + 120_000 });
  };
  const shared = lock();
  const [a, b] = tabs();
  const first = new Session(exchange, shared, a, () => now);
  const second = new Session(exchange, shared, b, () => now);
  const tokens = await Promise.all([first.token(), second.token()]);
  expect(tokens).toEqual(["t1", "t1"]);
  expect(exchanges).toBe(1);

  now += 120_000 - MARGIN_MS + 1;
  expect(await second.token()).toBe("t2");
  expect(first.usable()).toBe(true);
});

test("a session that has ended is ended in every tab", async () => {
  const [a, b] = tabs();
  const first = new Session(() => Promise.resolve(undefined), lock(), a);
  const second = new Session(() => Promise.resolve(undefined), lock(), b);
  expect(await first.token()).toBeUndefined();
  expect(first.signedOut()).toBe(true);
  expect(second.signedOut()).toBe(true);
});
