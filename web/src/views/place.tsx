// SPDX-License-Identifier: AGPL-3.0-or-later

// The places' routes, the same under a signed-in user's root and under a share link's `/g`:
// `/o/<id>` an organization, `/p/<id>` a project, `/w/<id>` a workload.

import type { ReactNode } from "react";
import type { Entry } from "./entries";
import { OrganizationPage, ProjectPage } from "./Places";
import { WorkloadPage } from "./WorkloadPage";

const KINDS: Record<string, Entry["kind"]> = { o: "organization", p: "project", w: "workload" };

/** The place `rest` names, a path below the base, if it names one. */
export function placeAt(rest: string): Entry | undefined {
  const found = /^\/([opw])\/([^/]+)$/.exec(rest);
  const kind = found === null ? undefined : KINDS[found[1] ?? ""];
  const id = found?.[2];
  return kind === undefined || id === undefined ? undefined : { kind, id };
}

/** The screen of `entry`. */
export function Place({ entry }: { readonly entry: Entry }): ReactNode {
  switch (entry.kind) {
    case "organization":
      return <OrganizationPage key={entry.id} id={entry.id} />;
    case "project":
      return <ProjectPage key={entry.id} id={entry.id} />;
    case "workload":
      return <WorkloadPage key={entry.id} id={entry.id} />;
  }
}
