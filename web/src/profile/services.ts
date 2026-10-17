// SPDX-License-Identifier: AGPL-3.0-or-later

// What the profile screens call: the profile and session services as the signed-in user, the
// account service for the ceremonies that need no credential (a sign-in challenge, a TOTP
// secret offered), and the browser's passkey ceremonies.

import type { Client } from "@connectrpc/connect";
import type { AccountService } from "../gen/korpis/v1/account_pb";
import type { ProfileService } from "../gen/korpis/v1/profile_pb";
import type { SessionService } from "../gen/korpis/v1/session_pb";
import type { Assertion, Creation, Registered } from "../session/webauthn";

/** The services and ceremonies of the profile screens. */
export interface ProfileServices {
  readonly profiles: Client<typeof ProfileService>;
  readonly sessions: Client<typeof SessionService>;
  readonly accounts: Client<typeof AccountService>;
  readonly assert: (challenge: Uint8Array, rpId: string) => Promise<Assertion>;
  readonly create: (c: Creation) => Promise<Registered>;
}
