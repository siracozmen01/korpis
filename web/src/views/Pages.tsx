// SPDX-License-Identifier: AGPL-3.0-or-later

// A list read a page at a time (PageRequest and PageResponse of api.proto): the first page, then
// each next one when asked for. A list that stopped at its first page without saying so would
// show less than there is as if it were all, so while a next page exists the list offers it.

import { useCallback, useState, type ReactNode } from "react";
import { Button } from "react-aria-components";
import { useLoad } from "./load";
import { refusal } from "./refusal";
import { Shown } from "./Shown";

/** One page of a list, and the token of the next; empty at the end. */
export interface Page<T> {
  readonly items: readonly T[];
  readonly next: string;
}

/** How many of a list one read asks for. */
export const PAGE_SIZE = 100;

/** The list `load` reads, one page per token, its items shown by `item`. */
export function Pages<T>(props: {
  readonly load: (token: string) => Promise<Page<T>>;
  readonly what: string;
  readonly title: string;
  readonly item: (value: T) => ReactNode;
  readonly empty: string;
}) {
  const { load } = props;
  const first = useLoad(useCallback(() => load(""), [load]));
  const [rest, setRest] = useState<{ readonly items: readonly T[]; readonly next?: string }>({
    items: [],
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const more = (token: string) => {
    setBusy(true);
    setError(undefined);
    load(token)
      .then(
        (page) => {
          setRest((r) => ({ items: [...r.items, ...page.items], next: page.next }));
        },
        (e: unknown) => {
          setError(refusal(e));
        },
      )
      .finally(() => {
        setBusy(false);
      });
  };
  return (
    <Shown
      loaded={first}
      what={props.what}
      done={(page) => {
        const items = [...page.items, ...rest.items];
        const next = rest.next ?? page.next;
        return (
          <section aria-label={props.title}>
            <h2>{props.title}</h2>
            <ul>
              {items.length === 0 && <li>{props.empty}</li>}
              {items.map((i) => props.item(i))}
            </ul>
            {next !== "" && (
              <Button
                isDisabled={busy}
                onPress={() => {
                  more(next);
                }}
              >
                Show more
              </Button>
            )}
            {error !== undefined && <p role="alert">{error}</p>}
          </section>
        );
      }}
    />
  );
}
