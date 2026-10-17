// SPDX-License-Identifier: AGPL-3.0-or-later

import { render, screen } from "@testing-library/react";
import { App, returnTo } from "./App";
import { Session, pageLock } from "./session/session";

const quiet = { post: () => undefined, listen: () => undefined };

test("signed out, the shell leads to signing in", async () => {
  window.history.replaceState(null, "", "/");
  const signedOut = new Session(() => Promise.resolve(undefined), pageLock(), quiet);
  render(<App session={signedOut} />);
  expect(await screen.findByRole("heading", { name: "Sign in" })).toBeTruthy();
  expect(window.location.pathname).toBe("/sign-in");
});

test("a control plane that cannot be reached is said to be, not shown as signed out", async () => {
  window.history.replaceState(null, "", "/");
  const unreachable = new Session(() => Promise.reject(new Error("down")), pageLock(), quiet);
  render(<App session={unreachable} />);
  expect(await screen.findByRole("alert")).toHaveProperty(
    "textContent",
    "The control plane cannot be reached.",
  );
});

test("signed out, a page asked for is returned to once signed in", async () => {
  window.history.replaceState(null, "", "/device?code=BCDF-GHJK");
  const signedOut = new Session(() => Promise.resolve(undefined), pageLock(), quiet);
  render(<App session={signedOut} />);
  expect(await screen.findByRole("heading", { name: "Sign in" })).toBeTruthy();
  expect(window.location.search).toBe("?then=%2Fdevice%3Fcode%3DBCDF-GHJK");
  expect(returnTo(window.location.search)).toBe("/device?code=BCDF-GHJK");
});

test("signing in returns only to a path of the panel's own", () => {
  for (const then of ["https://elsewhere.example", "//elsewhere.example", "/\\elsewhere", ""]) {
    expect(returnTo(`?then=${encodeURIComponent(then)}`)).toBe("/");
  }
  expect(returnTo("")).toBe("/");
});
