// SPDX-License-Identifier: AGPL-3.0-or-later

import { describe, expect, it } from "vitest";
import { challenge, verifier } from "./pkce";

describe("pkce", () => {
  it("makes the challenge RFC 7636 gives for its own verifier", async () => {
    const answer = await challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
    expect(answer).toBe("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
  });

  it("makes a new 43-character verifier of the unreserved alphabet each time", () => {
    const [a, b] = [verifier(), verifier()];
    expect(a).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(a).not.toBe(b);
  });
});
