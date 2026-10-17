// SPDX-License-Identifier: AGPL-3.0-or-later

// The browser's session (§9.4 of docs/08-identity.md): an access token exchanged for the refresh
// cookie, which the browser holds and no script can read. Each exchange rotates the cookie, and a
// cookie presented twice ends the session, so one tab exchanges at a time, under a lock of the
// browser's, and shares the token it gets with the other tabs, which then need not exchange at
// all. A token is renewed half a minute before it expires.

/** An access token, and when it expires, in milliseconds since the epoch. */
export interface Access {
  readonly token: string;
  readonly expiresAt: number;
}

/** What one tab tells the others: a token it got, or that the session has ended. */
export type Shared =
  { readonly kind: "access"; readonly access: Access } | { readonly kind: "ended" };

/** The browser's lock, `navigator.locks`, or a stand-in for it. */
export interface Locks {
  request<T>(name: string, run: () => Promise<T>): Promise<T>;
}

/** The channel the tabs share, a `BroadcastChannel`, or a stand-in for it. */
export interface Channel {
  post(message: Shared): void;
  listen(receive: (message: Shared) => void): void;
}

/** How long before it expires a token is renewed rather than presented. */
export const MARGIN_MS = 30_000;
/** The lock the tabs take turns exchanging under. */
export const LOCK = "korpis.session.exchange";

/** One tab's view of the session. */
export class Session {
  private access: Access | undefined;
  private ended = false;
  private readonly watchers = new Set<() => void>();

  constructor(
    private readonly exchange: () => Promise<Access | undefined>,
    private readonly locks: Locks,
    private readonly channel: Channel,
    private readonly now: () => number = Date.now,
  ) {
    channel.listen((message) => {
      if (message.kind === "access") this.set(message.access);
      else this.clear();
    });
  }

  /** The token to present now, renewed first if it is about to expire; none when signed out. */
  async token(): Promise<string | undefined> {
    if (this.usable()) return this.access?.token;
    return (await this.renew())?.token;
  }

  /** Whether a token is held that may still be presented. */
  usable(): boolean {
    return this.access !== undefined && this.access.expiresAt - MARGIN_MS > this.now();
  }

  /** Whether the session is known to have ended, or never begun. */
  signedOut(): boolean {
    return this.ended && this.access === undefined;
  }

  /** Exchanges the cookie under the lock, unless another tab already did while this one waited. */
  async renew(): Promise<Access | undefined> {
    return this.locks.request(LOCK, async () => {
      if (this.usable()) return this.access;
      const got = await this.exchange();
      if (got === undefined) {
        this.clear();
        this.channel.post({ kind: "ended" });
      } else {
        this.set(got);
        this.channel.post({ kind: "access", access: got });
      }
      return got;
    });
  }

  /** Forgets the session here and in every other tab, after the cookie is cleared. */
  end(): void {
    this.clear();
    this.channel.post({ kind: "ended" });
  }

  /** Calls `watch` whenever the session changes; the answer stops it. */
  subscribe(watch: () => void): () => void {
    this.watchers.add(watch);
    return () => this.watchers.delete(watch);
  }

  private set(access: Access): void {
    this.access = access;
    this.ended = false;
    this.notify();
  }

  private clear(): void {
    this.access = undefined;
    this.ended = true;
    this.notify();
  }

  private notify(): void {
    for (const watch of this.watchers) watch();
  }
}

/** A lock held within this page alone: one exchange at a time here, which is all a browser
 * without Web Locks allows, and what a page with one tab needs. */
export function pageLock(): Locks {
  let held: Promise<unknown> = Promise.resolve();
  return {
    request: <T>(_name: string, run: () => Promise<T>) => {
      const next = held.then(run, run);
      held = next.catch(() => undefined);
      return next;
    },
  };
}

/** The browser's own lock and channel, for a session shared by every tab of the panel. Without
 * Web Locks, tabs could exchange at once and end the session; such a browser keeps one tab. */
export function browserShared(): { locks: Locks; channel: Channel } {
  const broadcast = new BroadcastChannel("korpis.session");
  const webLocks = "locks" in navigator ? navigator.locks : undefined;
  return {
    locks:
      webLocks === undefined ? pageLock() : { request: (name, run) => webLocks.request(name, run) },
    channel: {
      post: (message) => {
        broadcast.postMessage(message);
      },
      listen: (receive) => {
        broadcast.onmessage = (event: MessageEvent<Shared>) => {
          receive(event.data);
        };
      },
    },
  };
}
