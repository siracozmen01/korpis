// SPDX-License-Identifier: AGPL-3.0-or-later

// What a refused call tells its user (§5 of docs/10-api.md): the reason the API gave in its own
// words where one is shown, and for a refused credential nothing about which part was wrong. A
// screen that branches on a refusal reads the `korpis.v1.Error` detail's reason (§8 of
// docs/10-api.md), never the message.

import { Code, ConnectError } from "@connectrpc/connect";
import { ErrorSchema } from "../gen/korpis/v1/common_pb";
import { LOST_BEFORE } from "../retrying";
import { t } from "../i18n/intl";

/** The reason of the refusal's `korpis.v1.Error` detail, such as `session.not_recent`. */
export function reasonOf(e: unknown): string | undefined {
  return ConnectError.from(e).findDetails(ErrorSchema)[0]?.reason;
}

/** The sentence a refusal is shown as. */
export function refusal(e: unknown): string {
  const error = ConnectError.from(e);
  switch (error.code) {
    case Code.Unauthenticated:
      return t({ defaultMessage: "That did not sign you in.", id: "e9SAPfzd" });
    case Code.ResourceExhausted: {
      const after = error.metadata.get("retry-after");
      const seconds = after === null ? NaN : Number(after);
      return Number.isFinite(seconds)
        ? t(
            {
              defaultMessage:
                "Too many attempts. Try again in {seconds, plural, one {# second} other {# seconds}}.",
              id: "9Z1gVcoX",
            },
            { seconds },
          )
        : t({ defaultMessage: "Too many attempts. Wait a little and try again.", id: "BZ0iK1XF" });
    }
    case Code.Unavailable:
      return t({
        defaultMessage: "The control plane cannot be reached. Try again.",
        id: "1UtyFZcc",
      });
    default:
      // Said here rather than in the control plane's words: what a session begun at a portal
      // may not do is reached from the installation's own address (§9.8 of
      // docs/08-identity.md).
      if (reasonOf(e) === "session.portal_confined") {
        return t({
          defaultMessage:
            "Signed in here, you cannot do this, as it reaches beyond this organization. Sign in at your provider's own address to do it.",
          id: "ngWBuQqU",
        });
      }
      return error.rawMessage;
  }
}

/** Whether `e` is a call whose answer may never have reached this browser: the network failed
 * it, before any answer or partway through one, after the panel's retries (`retrying.ts`); or a
 * retry was refused after an attempt whose answer was lost. A ceremony that spends a single-use
 * proof may then have done what it was asked, and sending it again is refused, so its screen
 * says what may have happened rather than "Failed to fetch" or the retry's refusal. */
export function lostAnswer(e: unknown): boolean {
  const error = ConnectError.from(e);
  if (error.metadata.get(LOST_BEFORE) !== null) return true;
  return error.code === Code.Unknown && error.cause instanceof TypeError;
}
