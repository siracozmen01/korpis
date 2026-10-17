// SPDX-License-Identifier: AGPL-3.0-or-later

import { createRouterTransport, createClient } from "@connectrpc/connect";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { expect, test } from "vitest";
import { clientsOver, ReachProvider } from "../clients";
import { OrganizationService } from "../gen/korpis/v1/api_pb";
import { RelayService } from "../gen/korpis/v1/relay_pb";
import type { SetThemeRequest } from "../gen/korpis/v1/api_pb";
import { ThemeEditor } from "./ThemeEditor";

function setup(setOn: string) {
  const saved: SetThemeRequest[] = [];
  const transport = createRouterTransport(({ service }) => {
    service(OrganizationService, {
      getTheme: () => ({
        governing: {
          setOn,
          ownVersion: setOn === "org_1" ? 3n : 0n,
          theme: { productName: "Acme" },
        },
      }),
      setTheme: (r) => {
        saved.push(r);
        return { governing: {} };
      },
    });
  });
  render(
    <ReachProvider
      value={{
        clients: clientsOver(transport),
        base: "",
        metered: false,
        relay: () => Promise.resolve(createClient(RelayService, transport)),
      }}
    >
      <ThemeEditor id="org_1" />
    </ReachProvider>,
  );
  return saved;
}

test("an organization's own theme is edited and saved against the version it was read at", async () => {
  const saved = setup("org_1");
  const name = await screen.findByLabelText("Product name");
  expect((name as HTMLInputElement).value).toBe("Acme");
  fireEvent.change(name, { target: { value: "Acme Hosting" } });
  const [accent] = screen.getAllByLabelText("Accent (#rrggbb)");
  if (accent === undefined) throw new Error("no accent field");
  fireEvent.change(accent, { target: { value: "#1f5fbf" } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => {
    expect(saved).toHaveLength(1);
  });
  expect(saved[0]?.mutation?.expectedVersion).toBe(3n);
  expect(saved[0]?.theme?.productName).toBe("Acme Hosting");
  expect(saved[0]?.theme?.light?.accent).toBe("#1f5fbf");
  expect(screen.getByRole("button", { name: "Remove this organization's theme" })).toBeTruthy();
});

test("an organization wearing an ancestor's theme starts from nothing and has none to remove", async () => {
  setup("org_root");
  const name = await screen.findByLabelText("Product name");
  expect((name as HTMLInputElement).value).toBe("");
  expect(screen.getByText(/nearest organization above/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Remove this organization's theme" })).toBeNull();
});
