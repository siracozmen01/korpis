// SPDX-License-Identifier: AGPL-3.0-or-later

// What a refused call tells its user (§5 of docs/10-api.md): the reason the API gave in its own
// words where one is shown, and for a refused credential nothing about which part was wrong. A
// screen that branches on a refusal reads the `korpis.v1.Error` detail's reason (§8 of
// docs/10-api.md), never the message.

import { Code, ConnectError } from "@connectrpc/connect";
import { ErrorSchema } from "../gen/korpis/v1/common_pb";

/** The reason of the refusal's `korpis.v1.Error` detail, such as `session.not_recent`. */
export function reasonOf(e: unknown): string | undefined {
  return ConnectError.from(e).findDetails(ErrorSchema)[0]?.reason;
}

/** The sentence a refusal is shown as. */
export function refusal(e: unknown): string {
  const error = ConnectError.from(e);
  switch (error.code) {
    case Code.Unauthenticated:
      return "That did not sign you in.";
    case Code.ResourceExhausted: {
      const after = error.metadata.get("retry-after");
      return after === null
        ? "Too many attempts. Wait a little and try again."
        : `Too many attempts. Try again in ${after} seconds.`;
    }
    case Code.Unavailable:
      return "The control plane cannot be reached. Try again.";
    default:
      return error.rawMessage;
  }
}
