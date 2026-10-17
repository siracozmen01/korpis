// SPDX-License-Identifier: AGPL-3.0-or-later

// The navigation (§5 of docs/11-surface-web.md): the places the caller's own grants open, then
// within an organization its organizations and projects, and within a project its workloads,
// each list holding only what the caller may read. Nothing is shown as locked; what the caller
// may not read is simply not there.

import { useCallback } from "react";
import { Link } from "react-aria-components";
import { useReach } from "../clients";
import { type Entry, entries, pathOf } from "./entries";
import { useLoad } from "./load";
import { PAGE_SIZE, Pages } from "./Pages";
import { Shown } from "./Shown";
import { CONVERGENCE } from "../workload/words";

/** The places the caller's grants open. */
export function Home() {
  const { clients } = useReach();
  const load = useCallback(async () => {
    // Every grant held, not a first page of them: the places are what all of them open.
    const held = [];
    let pageToken = "";
    do {
      const r = await clients.grants.listGrants({ page: { pageSize: PAGE_SIZE, pageToken } });
      held.push(...r.grants);
      pageToken = r.page?.nextPageToken ?? "";
    } while (pageToken !== "");
    return entries(held);
  }, [clients]);
  const loaded = useLoad(load);
  return (
    <section aria-labelledby="reach">
      <h2 id="reach">What you can reach</h2>
      <Shown
        loaded={loaded}
        what="what you can reach"
        done={(found) =>
          found.length === 0 ? (
            <p>No grant you hold opens an organization, a project, or a workload yet.</p>
          ) : (
            <ul>
              {found.map((e) => (
                <EntryItem key={`${e.kind}:${e.id}`} entry={e} />
              ))}
            </ul>
          )
        }
      />
    </section>
  );
}

/** One place, by its name; left out if it cannot be read. */
function EntryItem({ entry }: { readonly entry: Entry }) {
  const { clients, base } = useReach();
  const load = useCallback(async () => {
    const id = entry.id;
    switch (entry.kind) {
      case "organization":
        return (await clients.organizations.getOrganization({ id })).organization?.name ?? id;
      case "project":
        return (await clients.projects.getProject({ id })).project?.name ?? id;
      case "workload":
        return (await clients.workloads.getWorkload({ id })).status?.workload?.name ?? id;
    }
  }, [clients, entry]);
  const loaded = useLoad(load);
  return (
    <Shown
      loaded={loaded}
      what={`the ${entry.kind}`}
      done={(name) => (
        <li>
          <Link href={pathOf(base, entry)}>{name}</Link> <small>{entry.kind}</small>
        </li>
      )}
    />
  );
}

/** An organization: its name, the organizations under it, and its projects. */
export function OrganizationPage({ id }: { readonly id: string }) {
  const { clients, base } = useReach();
  const { organizations, projects } = clients;
  const name = useLoad(
    useCallback(
      async () => (await organizations.getOrganization({ id })).organization,
      [organizations, id],
    ),
  );
  const children = useCallback(
    async (pageToken: string) => {
      const r = await organizations.listOrganizations({
        parent: id,
        page: { pageSize: PAGE_SIZE, pageToken },
      });
      return { items: r.organizations, next: r.page?.nextPageToken ?? "" };
    },
    [organizations, id],
  );
  const inside = useCallback(
    async (pageToken: string) => {
      const r = await projects.listProjects({
        organization: id,
        page: { pageSize: PAGE_SIZE, pageToken },
      });
      return { items: r.projects, next: r.page?.nextPageToken ?? "" };
    },
    [projects, id],
  );
  return (
    <main>
      <Shown loaded={name} what="the organization" done={(o) => <h1>{o?.name ?? id}</h1>} />
      <Pages
        load={children}
        what="its organizations"
        title="Organizations"
        empty="None you can read."
        item={(o) => (
          <li key={o.id}>
            <Link href={pathOf(base, { kind: "organization", id: o.id })}>{o.name}</Link>
          </li>
        )}
      />
      <Pages
        load={inside}
        what="its projects"
        title="Projects"
        empty="None you can read."
        item={(p) => (
          <li key={p.id}>
            <Link href={pathOf(base, { kind: "project", id: p.id })}>{p.name}</Link>
          </li>
        )}
      />
    </main>
  );
}

/** A project: its name and its workloads. */
export function ProjectPage({ id }: { readonly id: string }) {
  const { clients, base } = useReach();
  const { projects, workloads } = clients;
  const name = useLoad(
    useCallback(async () => (await projects.getProject({ id })).project, [projects, id]),
  );
  const inside = useCallback(
    async (pageToken: string) => {
      const r = await workloads.listWorkloads({
        project: id,
        page: { pageSize: PAGE_SIZE, pageToken },
      });
      const items = r.workloads.flatMap((s) =>
        s.workload === undefined ? [] : [{ workload: s.workload, convergence: s.convergence }],
      );
      return { items, next: r.page?.nextPageToken ?? "" };
    },
    [workloads, id],
  );
  return (
    <main>
      <Shown loaded={name} what="the project" done={(p) => <h1>{p?.name ?? id}</h1>} />
      <Pages
        load={inside}
        what="its workloads"
        title="Workloads"
        empty="None you can read."
        item={({ workload: w, convergence }) => (
          <li key={w.id}>
            <Link href={pathOf(base, { kind: "workload", id: w.id })}>{w.name}</Link>{" "}
            <small>{CONVERGENCE[convergence]}</small>
          </li>
        )}
      />
    </main>
  );
}
