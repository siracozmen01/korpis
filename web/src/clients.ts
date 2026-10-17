// SPDX-License-Identifier: AGPL-3.0-or-later

// The clients a screen reads and acts through, and the path its links are written under. A
// signed-in user's calls carry the session's access token; a share link's carry the link itself
// and nothing else (§6 of docs/11-surface-web.md), so the same screens serve both, each showing
// only what its own credential reaches.

import { createClient, type Client, type Interceptor, type Transport } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { createContext, useContext } from "react";
import {
  GrantService,
  OrganizationService,
  ProjectService,
  WorkloadService,
} from "./gen/korpis/v1/api_pb";

/** The services the navigation reads. */
export interface Clients {
  readonly grants: Client<typeof GrantService>;
  readonly organizations: Client<typeof OrganizationService>;
  readonly projects: Client<typeof ProjectService>;
  readonly workloads: Client<typeof WorkloadService>;
}

/** The clients of every service over `transport`. */
export function clientsOver(transport: Transport): Clients {
  return {
    grants: createClient(GrantService, transport),
    organizations: createClient(OrganizationService, transport),
    projects: createClient(ProjectService, transport),
    workloads: createClient(WorkloadService, transport),
  };
}

/** Presents `credential` as the bearer of every call. */
function presenting(credential: string): Interceptor {
  return (next) => (request) => {
    request.header.set("Authorization", `Bearer ${credential}`);
    return next(request);
  };
}

/** A transport to `baseUrl` whose calls carry a share link and no cookie. */
export function linkTransport(baseUrl: string, link: string): Transport {
  const withoutCookies: typeof fetch = (input, init) =>
    fetch(input, { ...init, credentials: "omit" });
  return createConnectTransport({
    baseUrl,
    fetch: withoutCookies,
    interceptors: [presenting(link)],
  });
}

/** What a screen reaches the API through, and the path its own links begin with. */
export interface Reach {
  readonly clients: Clients;
  readonly base: string;
}

const ReachContext = createContext<Reach | undefined>(undefined);

/** Provides the clients and the base path to the screens below it. */
export const ReachProvider = ReachContext.Provider;

/** The clients and base path the screen was given. */
export function useReach(): Reach {
  const reach = useContext(ReachContext);
  if (reach === undefined) throw new Error("a screen was rendered without its clients");
  return reach;
}
