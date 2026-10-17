// SPDX-License-Identifier: AGPL-3.0-or-later

import { expect, test } from "vitest";
import { route } from "./routing";

const scope = "https://panel.example/";
const shell = new Set(["assets/index-abc123.js", "assets/index-def456.css", "index.html"]);
const get = (path: string, mode = "cors") => ({ method: "GET", url: scope + path, mode });

test("a file of the bundle is the cache's, a page the network's first, and the API never", () => {
  expect(route(get("assets/index-abc123.js"), scope, shell)).toBe("shell");
  expect(route(get("w/wl_1", "navigate"), scope, shell)).toBe("page");
  expect(route(get("", "navigate"), scope, shell)).toBe("page");
  const call = {
    method: "POST",
    url: `${scope}korpis.v1.WorkloadService/GetWorkload`,
    mode: "cors",
  };
  expect(route(call, scope, shell), "an API call").toBe("network");
  expect(route(get("korpis.v1.WorkloadService/GetWorkload"), scope, shell)).toBe("network");
  expect(route(get("about/source", "navigate"), scope, shell), "the source notice").toBe("network");
  expect(route(get("assets/other.js"), scope, shell), "not of this bundle").toBe("network");
  const elsewhere = { method: "GET", url: "https://relay.example/x", mode: "navigate" };
  expect(route(elsewhere, scope, shell), "another origin").toBe("network");
});
