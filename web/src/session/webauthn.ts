// SPDX-License-Identifier: AGPL-3.0-or-later

// A passkey ceremony in the browser (§9.6 of docs/08-identity.md): the options the control plane
// gives, as `navigator.credentials` takes them, and its answer as the control plane reads it.
// Every byte travels as bytes; nothing is re-encoded on the way.

import { t } from "../i18n/intl";

/** What `navigator.credentials.get` answered, as `SignInWithPasskey` takes it. */
export interface Assertion {
  readonly credentialId: Uint8Array;
  readonly clientDataJson: Uint8Array;
  readonly authenticatorData: Uint8Array;
  readonly signature: Uint8Array;
}

/** What `navigator.credentials.create` answered, as a registration takes it. */
export interface Registered {
  readonly clientDataJson: Uint8Array;
  readonly attestationObject: Uint8Array;
}

/** The algorithms the control plane accepts (§9.6): ES256, EdDSA, RS256. */
const ALGORITHMS: PublicKeyCredentialParameters[] = [
  { type: "public-key", alg: -7 },
  { type: "public-key", alg: -8 },
  { type: "public-key", alg: -257 },
];

function bytes(buffer: ArrayBuffer): Uint8Array {
  return new Uint8Array(buffer);
}

/** Asks the browser's authenticators to answer `challenge` for the site `rpId`. */
export async function assert(challenge: Uint8Array, rpId: string): Promise<Assertion> {
  const credential = await navigator.credentials.get({
    publicKey: {
      challenge: new Uint8Array(challenge),
      rpId,
      userVerification: "required",
      timeout: 120_000,
    },
  });
  if (!(credential instanceof PublicKeyCredential)) {
    throw new Error(t({ defaultMessage: "no passkey answered", id: "L/Pkbpdp" }));
  }
  const response = credential.response as AuthenticatorAssertionResponse;
  return {
    credentialId: bytes(credential.rawId),
    clientDataJson: bytes(response.clientDataJSON),
    authenticatorData: bytes(response.authenticatorData),
    signature: bytes(response.signature),
  };
}

/** What a registration asks the browser for, as the control plane gives it. */
export interface Creation {
  readonly challenge: Uint8Array;
  readonly rpId: string;
  readonly userId: Uint8Array;
  readonly userName: string;
  readonly displayName: string;
  readonly exclude: readonly Uint8Array[];
}

/** Asks the browser to make a passkey for `c`, resident and verified. */
export async function create(c: Creation): Promise<Registered> {
  const credential = await navigator.credentials.create({
    publicKey: {
      challenge: new Uint8Array(c.challenge),
      rp: { id: c.rpId, name: c.rpId },
      user: { id: new Uint8Array(c.userId), name: c.userName, displayName: c.displayName },
      pubKeyCredParams: ALGORITHMS,
      excludeCredentials: c.exclude.map((id) => ({ type: "public-key", id: new Uint8Array(id) })),
      authenticatorSelection: { residentKey: "required", userVerification: "required" },
      attestation: "none",
      timeout: 120_000,
    },
  });
  if (!(credential instanceof PublicKeyCredential)) {
    throw new Error(t({ defaultMessage: "no passkey was made", id: "5ot6y0in" }));
  }
  const response = credential.response as AuthenticatorAttestationResponse;
  return {
    clientDataJson: bytes(response.clientDataJSON),
    attestationObject: bytes(response.attestationObject),
  };
}
