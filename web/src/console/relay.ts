// SPDX-License-Identifier: AGPL-3.0-or-later

// A session with a workload through a relay, as a browser holds one ("From a browser" in §3.1 of
// docs/02-architecture.md): a capability token minted for the action, `Watch` to open it and
// read what the node sends, and `Send` for what the client sends, numbered so a retried call
// never repeats a frame. The token is renewed through the session before it expires; a renewal
// spends no use of a share link's grant, since the session's opening spent the one it costs.

import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import { timestampMs } from "@bufbuild/protobuf/wkt";
import type { Client } from "@connectrpc/connect";
import type { Clients } from "../clients";
import {
  ConsoleChunkSchema,
  ConsoleReadSchema,
  type ConsoleChunk,
} from "../gen/korpis/v1/console_pb";
import type { RelayService, SessionFrame, SessionFrameSchema } from "../gen/korpis/v1/relay_pb";

/** The relay a session is opened at. */
export type Relay = Client<typeof RelayService>;

/** How long before its token expires a session renews it. */
const EARLY_MS = 30_000;

/** What opening a session needs. */
export interface Opening {
  readonly clients: Clients;
  readonly relay: Relay;
  readonly workload: string;
  readonly action: string;
  readonly params: Uint8Array;
  readonly signal: AbortSignal;
}

/** An open session, while its watch runs: sending into it, and its token kept renewed. */
export class Session {
  #sent = 0n;
  #token: string;
  #renewal: ReturnType<typeof setTimeout> | undefined;

  constructor(
    private readonly opening: Opening,
    token: string,
    private readonly id: string,
    private readonly key: string,
  ) {
    this.#token = token;
  }

  /** Sends `frames` into the session, in order, once. */
  async send(frames: MessageInitShape<typeof SessionFrameSchema>[]): Promise<void> {
    const first = this.#sent;
    const r = await this.opening.relay.send(
      { session: this.id, sendKey: this.key, first, frames },
      { signal: this.opening.signal },
    );
    this.#sent = r.delivered;
  }

  /** Renews the token at `expiresAt` less a margin, and again after each renewal. */
  renewBefore(expiresAt: number): void {
    clearTimeout(this.#renewal);
    const wait = Math.max(0, expiresAt - Date.now() - EARLY_MS);
    this.#renewal = setTimeout(() => {
      void this.renew();
    }, wait);
  }

  private async renew(): Promise<void> {
    const { clients, workload, action } = this.opening;
    const minted = await clients.grants.mintToken({
      actions: [action],
      scope: { kind: { case: "workload", value: workload } },
      renews: this.#token,
    });
    this.#token = minted.token;
    await this.send([{ frame: { case: "renew", value: minted.token } }]);
    if (minted.expiresAt !== undefined) this.renewBefore(timestampMs(minted.expiresAt));
  }

  /** Stops renewing. */
  stop(): void {
    clearTimeout(this.#renewal);
  }
}

/** What a session's watch yields: its opening, then each data frame. */
export type Heard =
  | { readonly kind: "opened"; readonly session: Session }
  | { readonly kind: "data"; readonly data: Uint8Array }
  | { readonly kind: "end"; readonly code: string; readonly message: string };

/** Opens a session for `opening` and yields what it hears until it ends. */
export async function* open(opening: Opening): AsyncGenerator<Heard> {
  const { clients, relay, workload, action, params, signal } = opening;
  const minted = await clients.grants.mintToken({
    actions: [action],
    scope: { kind: { case: "workload", value: workload } },
  });
  const stream = relay.watch(
    { token: minted.token, action, workload, route: minted.route, params },
    { signal },
  );
  let session: Session | undefined;
  try {
    for await (const r of stream) {
      const m = r.message;
      if (m.case === "opened") {
        session = new Session(opening, minted.token, m.value.session, m.value.sendKey);
        if (m.value.expiresAt !== undefined) session.renewBefore(timestampMs(m.value.expiresAt));
        yield { kind: "opened", session };
      } else if (m.case === "frame") {
        const heard = frameOf(m.value);
        if (heard !== undefined) yield heard;
        if (heard?.kind === "end") return;
      }
    }
  } finally {
    session?.stop();
  }
}

function frameOf(frame: SessionFrame): Heard | undefined {
  const f = frame.frame;
  if (f.case === "data") return { kind: "data", data: f.value };
  if (f.case === "end") return { kind: "end", code: f.value.code, message: f.value.message };
  return undefined;
}

/** The parameters of a console reading session. */
export function reading(init: MessageInitShape<typeof ConsoleReadSchema>): Uint8Array {
  return toBinary(ConsoleReadSchema, create(ConsoleReadSchema, init));
}

/** A console reading session's data frame. */
export function chunkOf(data: Uint8Array): ConsoleChunk {
  return fromBinary(ConsoleChunkSchema, data);
}
