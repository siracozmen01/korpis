// SPDX-License-Identifier: AGPL-3.0-or-later

// Typing into a console (§6 of docs/14-streams.md): a `workload.console.write` session, a
// separate action from reading so watching and typing are delegated separately, opened on the
// first line typed and kept while the screen is open. Each line is one ConsoleInput frame sent
// with `Send`, numbered, so a retried call never types it twice.

import { create, toBinary } from "@bufbuild/protobuf";
import { ConsoleInputSchema } from "../gen/korpis/v1/console_pb";
import { type Opening, type Session, open } from "./relay";

const WRITE = "workload.console.write";

/** A writer, opening its session when first asked to type. */
export class Writer {
  #session: Promise<Session> | undefined;
  #stop = new AbortController();

  constructor(
    private readonly opening: () => Promise<Omit<Opening, "action" | "params" | "signal">>,
  ) {}

  /** Types `line` and a line break into the console. */
  async type(line: string): Promise<void> {
    const session = await this.session();
    const input = create(ConsoleInputSchema, {
      input: { case: "data", value: new TextEncoder().encode(`${line}\n`) },
    });
    await session.send([{ frame: { case: "data", value: toBinary(ConsoleInputSchema, input) } }]);
  }

  private session(): Promise<Session> {
    this.#session ??= this.begin().catch((e: unknown) => {
      this.#session = undefined;
      throw e;
    });
    return this.#session;
  }

  private async begin(): Promise<Session> {
    const base = await this.opening();
    const heard = open({
      ...base,
      action: WRITE,
      params: new Uint8Array(),
      signal: this.#stop.signal,
    });
    const first = await heard.next();
    if (first.done === true || first.value.kind !== "opened")
      throw new Error("the console refused input");
    // The rest of the session is its end, if it ends: drained so the watch stays read, and a
    // session that ended, or was closed, is opened again by the next line typed.
    void (async () => {
      try {
        for await (const h of heard) if (h.kind === "end") break;
      } catch {
        // Closed with the screen, or dropped: nothing waits on it.
      }
      this.#session = undefined;
    })();
    return first.value.session;
  }

  /** Ends the writing session. */
  close(): void {
    this.#stop.abort();
  }
}
