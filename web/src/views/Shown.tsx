// SPDX-License-Identifier: AGPL-3.0-or-later

// What a screen shows for each state of a read (load.ts): a line while it is read, the thing
// once it is, nothing at all when it is not the caller's to see, and the failure when it failed.

import type { ReactNode } from "react";
import type { Loaded } from "./load";
import { refusal } from "./refusal";

/** `loaded` rendered by `done` once read. */
export function Shown<T>(props: {
  readonly loaded: Loaded<T>;
  readonly what: string;
  readonly done: (value: T) => ReactNode;
}) {
  const { loaded } = props;
  switch (loaded.state) {
    case "loading":
      return <p role="status">Reading {props.what}…</p>;
    case "hidden":
      return null;
    case "failed":
      return <p role="alert">{refusal(loaded.error)}</p>;
    case "done":
      return props.done(loaded.value);
  }
}
