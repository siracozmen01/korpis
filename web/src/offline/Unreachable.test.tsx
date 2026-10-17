// SPDX-License-Identifier: AGPL-3.0-or-later

import { render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { heard } from "./heard";
import { Unreachable } from "./Unreachable";

afterEach(() => {
  localStorage.clear();
  vi.useRealTimers();
});

test("an unreachable control plane leaves every value unknown, and says how long it has been", () => {
  heard(Date.now() - 125_000);
  render(<Unreachable retry={() => undefined} />);
  expect(screen.getByRole("alert").textContent).toBe("The control plane cannot be reached.");
  expect(screen.getByText(/Every value it holds is unknown/)).toBeTruthy();
  expect(screen.getByText("This browser last heard from it 2 min ago.")).toBeTruthy();
});

test("it asks again by itself, and never having heard is said too", () => {
  vi.useFakeTimers();
  const retry = vi.fn();
  render(<Unreachable retry={retry} />);
  expect(screen.getByText("This browser has not heard from it before.")).toBeTruthy();
  vi.advanceTimersByTime(31_000);
  expect(retry).toHaveBeenCalledTimes(2);
});
