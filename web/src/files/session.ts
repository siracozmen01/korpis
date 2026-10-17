// SPDX-License-Identifier: AGPL-3.0-or-later

// A file session as a browser holds one (§3.2 of docs/06-storage.md): a `workload.file.read` or
// `workload.file.write` session at a relay, each request one FileRequest frame sent with `Send`,
// each answer one FileResponse frame heard on the `Watch`, matched to its request by number.
// Requests may be sent ahead of their answers; they are sent one call at a time, in order, since
// the relay numbers what it delivered and a call sent beside another would repeat its numbers.

import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import {
  FileErrorCode,
  FileRequestSchema,
  FileResponseSchema,
  type FileResponse,
} from "../gen/korpis/v1/files_pb";
import { type Heard, type Opening, type Session, open } from "../console/relay";
import { t } from "../i18n/intl";

/** The action that reads. */
export const READ = "workload.file.read";
/** The action that writes, and reads. */
export const WRITE = "workload.file.write";

/** What a request asks, without its number. */
export type Asked = NonNullable<MessageInitShape<typeof FileRequestSchema>["request"]>;
/** What an answer says. */
export type Said = FileResponse["response"];

/** A request the node refused: the session goes on. */
export class Refused extends Error {
  constructor(
    readonly code: FileErrorCode,
    message: string,
  ) {
    super(message);
  }
}

/** The session ended, or never opened, with the relay's or the node's reason. */
export class Ended extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message === "" ? code : message);
  }
}

interface Waiting {
  readonly resolve: (said: Said) => void;
  readonly reject: (e: Error) => void;
}

/** An open file session. */
export class FileSession {
  #next = 0n;
  #waiting = new Map<bigint, Waiting>();
  #sending: Promise<void> = Promise.resolve();
  #ended: Ended | undefined;

  private constructor(
    private readonly session: Session,
    private readonly stop: AbortController,
    /** Whether it may change what it sees. */
    readonly writes: boolean,
  ) {}

  /** Opens a session on `opening.workload`, to write when `writes`. */
  static async open(
    opening: Omit<Opening, "action" | "params" | "signal">,
    writes: boolean,
  ): Promise<FileSession> {
    const stop = new AbortController();
    const heard = open({
      ...opening,
      action: writes ? WRITE : READ,
      params: new Uint8Array(),
      signal: stop.signal,
    });
    const first = await heard.next();
    if (first.done === true)
      throw new Ended(
        "unavailable",
        t({ defaultMessage: "the session did not open", id: "YB8VbPji" }),
      );
    if (first.value.kind === "end") throw new Ended(first.value.code, first.value.message);
    if (first.value.kind !== "opened")
      throw new Ended(
        "unavailable",
        t({ defaultMessage: "the session did not open", id: "YB8VbPji" }),
      );
    const s = new FileSession(first.value.session, stop, writes);
    void s.hear(heard);
    return s;
  }

  /** Reads the session's answers until it ends, settling each request it answers. */
  private async hear(heard: AsyncGenerator<Heard>): Promise<void> {
    let end = new Ended("closed", t({ defaultMessage: "the session was closed", id: "e/zsRyEz" }));
    try {
      for await (const h of heard) {
        if (h.kind === "end") {
          end = new Ended(h.code, h.message);
          break;
        }
        if (h.kind !== "data") continue;
        const answer = fromBinary(FileResponseSchema, h.data);
        const waiting = this.#waiting.get(answer.id);
        this.#waiting.delete(answer.id);
        waiting?.resolve(answer.response);
      }
    } catch {
      end = new Ended("unavailable", t({ defaultMessage: "the session was lost", id: "R4Vpa9Ew" }));
    }
    this.#ended = end;
    for (const w of this.#waiting.values()) w.reject(end);
    this.#waiting.clear();
  }

  /** Asks `request`; what the node answered, a refusal thrown as [`Refused`]. */
  async ask(request: Asked): Promise<Said> {
    if (this.#ended !== undefined) throw this.#ended;
    this.#next += 1n;
    const id = this.#next;
    const answered = new Promise<Said>((resolve, reject) => {
      this.#waiting.set(id, { resolve, reject });
    });
    const bytes = toBinary(FileRequestSchema, create(FileRequestSchema, { id, request }));
    const sent = this.#sending.then(() =>
      this.session.send([{ frame: { case: "data", value: bytes } }]),
    );
    this.#sending = sent.catch(() => undefined);
    try {
      await sent;
    } catch (e) {
      this.#waiting.delete(id);
      throw e;
    }
    const said = await answered;
    if (said.case === "error") throw new Refused(said.value.code, said.value.message);
    return said;
  }

  /** Ends the session. */
  close(): void {
    this.session.stop();
    this.stop.abort();
  }
}
