// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's way to the API (§1 of docs/11-surface-web.md): the generated clients of the
// published services over Connect, sent to the control plane the panel was served by, or to the
// one the build names when the panel is served from elsewhere. Credentials are included, since
// the session's refresh credential is a cookie (§9.4 of docs/08-identity.md). Two transports: an
// anonymous one, for the ceremonies and for exchanging the cookie itself, and one whose calls
// carry the session's access token.

import {
  Code,
  ConnectError,
  createClient,
  type Client,
  type Interceptor,
} from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { timestampMs } from "@bufbuild/protobuf/wkt";
import { type Clients, clientsOver, linkTransport } from "./clients";
import type { Relay } from "./console/relay";
import { AccountService } from "./gen/korpis/v1/account_pb";
import { ServerInfoService } from "./gen/korpis/v1/api_pb";
import { RelayService } from "./gen/korpis/v1/relay_pb";
import { ProfileService } from "./gen/korpis/v1/profile_pb";
import { SessionService } from "./gen/korpis/v1/session_pb";
import { type Access, Session, browserShared } from "./session/session";

/** The control plane: the build's `VITE_KORPIS_API`, or the origin the panel was served from. */
export function controlPlane(): string {
  const named = import.meta.env.VITE_KORPIS_API;
  return named !== undefined && named !== "" ? named : window.location.origin;
}

const withCookies: typeof fetch = (input, init) =>
  fetch(input, { ...init, credentials: "include" });

const anonymous = createConnectTransport({ baseUrl: controlPlane(), fetch: withCookies });

/** The server's description and the sign-in page's theme, asked with no credential. */
export const serverInfo: Client<typeof ServerInfoService> = createClient(
  ServerInfoService,
  anonymous,
);

/** The account service: redemption and sign-in, called with no credential. */
export const accounts: Client<typeof AccountService> = createClient(AccountService, anonymous);

/** The session service, called with no credential: the cookie's exchange and sign-out. */
export const cookieSessions: Client<typeof SessionService> = createClient(
  SessionService,
  anonymous,
);

/** The cookie exchanged for an access token; none once the session has ended. */
export async function exchange(): Promise<Access | undefined> {
  try {
    const answer = await cookieSessions.exchange({});
    const expires = answer.accessExpiresAt;
    const expiresAt = expires === undefined ? Date.now() : timestampMs(expires);
    return { token: answer.accessToken, expiresAt };
  } catch (e) {
    if (ConnectError.from(e).code === Code.Unauthenticated) return undefined;
    throw e;
  }
}

const shared = browserShared();

/** Every tab's session, shared through the browser's lock and channel. */
export const session = new Session(exchange, shared.locks, shared.channel);

/** Adds the session's access token to every call; a call made signed out goes without. */
export function bearer(of: Session): Interceptor {
  return (next) => async (request) => {
    const token = await of.token();
    if (token !== undefined) request.header.set("Authorization", `Bearer ${token}`);
    return next(request);
  };
}

/** The transport the signed-in panel calls the API through. */
export const signedIn = createConnectTransport({
  baseUrl: controlPlane(),
  fetch: withCookies,
  interceptors: [bearer(session)],
});

/** The session service, called as the signed-in user: sessions listed and ended, devices. */
export const sessions: Client<typeof SessionService> = createClient(SessionService, signedIn);

/** The profile service, called as the signed-in user: factors, and re-authenticating. */
export const profiles: Client<typeof ProfileService> = createClient(ProfileService, signedIn);

/** The navigation's clients, as the signed-in user. */
export const signedInClients: Clients = clientsOver(signedIn);

const linked = new Map<string, Clients>();

/** The navigation's clients as the holder of the share link `link`, made once per link. */
export function linkClients(link: string): Clients {
  let clients = linked.get(link);
  if (clients === undefined) {
    clients = clientsOver(linkTransport(controlPlane(), link));
    linked.set(link, clients);
  }
  return clients;
}

let found: Promise<Relay> | undefined;

/** The relay this panel opens sessions at: the control plane, when it serves the role, or the
 * first relay it names; asked once and remembered. A session carries its own token, so the
 * relay's transport sends no cookie and no bearer. */
export function relay(): Promise<Relay> {
  found ??= (async () => {
    const info = await serverInfo.getServerInfo({});
    const url = info.controlPlaneRelay ? controlPlane() : info.relays[0];
    if (url === undefined)
      throw new Error("no relay serves sessions: the control plane names none");
    const plain: typeof fetch = (input, init) => fetch(input, { ...init, credentials: "omit" });
    return createClient(RelayService, createConnectTransport({ baseUrl: url, fetch: plain }));
  })().catch((e: unknown) => {
    found = undefined;
    throw e;
  });
  return found;
}
