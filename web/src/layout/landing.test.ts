// SPDX-License-Identifier: AGPL-3.0-or-later

import { expect, test } from "vitest";
import { hostOf } from "./landing";

test("a link is written only for an https address with a host and no user information", () => {
  expect(hostOf("https://help.pinecone.example/desk")).toBe("help.pinecone.example");
  for (const url of [
    "javascript:alert(1)",
    "JAVASCRIPT:alert(1)",
    "data:text/html,<script>alert(1)</script>",
    "http://help.pinecone.example/",
    "https://bank.example@elsewhere.example/",
    "not an address",
  ]) {
    expect(hostOf(url), url).toBeUndefined();
  }
});
