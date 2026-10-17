// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's way to the API (§1 of docs/11-surface-web.md): the generated clients of the
// published services over Connect, sent to the control plane the panel was served by, or to the
// one the build names when the panel is served from elsewhere. Credentials are included, since
// the session's refresh credential is a cookie (§9.4 of docs/08-identity.md), which the browser
// sends only to the control plane's own site.

import { createClient, type Client } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { SessionService } from "./gen/korpis/v1/session_pb";

/** The control plane: the build's `VITE_KORPIS_API`, or the origin the panel was served from. */
export function controlPlane(): string {
  const named = import.meta.env.VITE_KORPIS_API;
  return named !== undefined && named !== "" ? named : window.location.origin;
}

const transport = createConnectTransport({
  baseUrl: controlPlane(),
  fetch: (input, init) => fetch(input, { ...init, credentials: "include" }),
});

/** The session service: exchanging the refresh cookie, and the device authorizations. */
export const sessions: Client<typeof SessionService> = createClient(SessionService, transport);
