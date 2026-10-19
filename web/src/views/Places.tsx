// SPDX-License-Identifier: AGPL-3.0-or-later

// The navigation (§5 of docs/11-surface-web.md): the places the caller's own grants open, then
// within an organization its organizations and projects, and within a project its workloads,
// each list holding only what the caller may read. Nothing is shown as locked; what the caller
// may not read is simply not there.

import { useCallback } from "react";
import { Link } from "react-aria-components";
import { useReach } from "../clients";
import { type Entry, entries, pathOf } from "./entries";
import { everyGrant } from "./held";
import { useLoad } from "./load";
import { PAGE_SIZE, Pages } from "./Pages";
import { Shown } from "./Shown";
import { Suspension } from "./Suspension";
import { OrganizationStatus } from "../gen/korpis/v1/tenancy_pb";
import { CONVERGENCE } from "../workload/words";
import { ThemeEditor } from "../theme/ThemeEditor";
import { t } from "../i18n/intl";

/** The places the caller's grants open. */
export function Home() {
  const { clients } = useReach();
  // Every grant held, not a first page of them: the places are what all of them open.
  const load = useCallback(async () => entries(await everyGrant(clients)), [clients]);
  const loaded = useLoad(load);
  return (
    <section aria-labelledby="reach">
      <h2 id="reach">{t({ defaultMessage: "What you can reach", id: "bBY+B+a1" })}</h2>
      <Shown
        loaded={loaded}
        reading={t({ defaultMessage: "Reading what you can reach…", id: "IyTtQLIW" })}
        done={(found) =>
          found.length === 0 ? (
            <p>
              {t({
                defaultMessage:
                  "No grant you hold opens an organization, a project, or a workload yet.",
                id: "jURFdkY7",
              })}
            </p>
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
      reading={t(
        {
          defaultMessage:
            "{kind, select, organization {Reading the organization…} project {Reading the project…} other {Reading the workload…}}",
          id: "nXNb6xjd",
        },
        { kind: entry.kind },
      )}
      done={(name) => (
        <li>
          <Link href={pathOf(base, entry)}>{name}</Link>
          <small className="aside">
            {t(
              {
                defaultMessage:
                  "{kind, select, organization {organization} project {project} other {workload}}",
                id: "u6Mspyx9",
                description: "the kind of a place the viewer can reach",
              },
              { kind: entry.kind },
            )}
          </small>
        </li>
      )}
    />
  );
}

/** An organization: its name, whether it is suspended, the organizations under it, and its
 * projects. */
export function OrganizationPage({ id }: { readonly id: string }) {
  const { clients, base, metered } = useReach();
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
      <Shown
        loaded={name}
        reading={t({ defaultMessage: "Reading the organization…", id: "R+2+cn8K" })}
        done={(o) => (
          <>
            <h1>{o?.name ?? id}</h1>
            {o !== undefined && <Suspension organization={o} />}
          </>
        )}
      />
      <Pages
        load={children}
        reading={t({ defaultMessage: "Reading its organizations…", id: "1YKny+jf" })}
        title={t({ defaultMessage: "Organizations", id: "JD2pwHED" })}
        empty={t({ defaultMessage: "None you can read.", id: "hybHBiuq" })}
        item={(o) => (
          <li key={o.id}>
            <Link href={pathOf(base, { kind: "organization", id: o.id })}>{o.name}</Link>
            {o.status === OrganizationStatus.SUSPENDED && (
              <small className="aside">
                {t({
                  defaultMessage: "suspended",
                  id: "Ceot6AFe",
                  description: "an organization's state",
                })}
              </small>
            )}
          </li>
        )}
      />
      <Pages
        load={inside}
        reading={t({ defaultMessage: "Reading its projects…", id: "TVvaeAem" })}
        title={t({ defaultMessage: "Projects", id: "UxTJRaKa" })}
        empty={t({ defaultMessage: "None you can read.", id: "hybHBiuq" })}
        item={(p) => (
          <li key={p.id}>
            <Link href={pathOf(base, { kind: "project", id: p.id })}>{p.name}</Link>
          </li>
        )}
      />
      {!metered && <ThemeEditor id={id} />}
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
      <Shown
        loaded={name}
        reading={t({ defaultMessage: "Reading the project…", id: "ma+8ta3Y" })}
        done={(p) => <h1>{p?.name ?? id}</h1>}
      />
      <Pages
        load={inside}
        reading={t({ defaultMessage: "Reading its workloads…", id: "IfkE03As" })}
        title={t({ defaultMessage: "Workloads", id: "BeBCMfk4" })}
        empty={t({ defaultMessage: "None you can read.", id: "hybHBiuq" })}
        item={({ workload: w, convergence }) => (
          <li key={w.id}>
            <Link href={pathOf(base, { kind: "workload", id: w.id })}>{w.name}</Link>
            <small className="aside">{CONVERGENCE[convergence]}</small>
          </li>
        )}
      />
    </main>
  );
}
