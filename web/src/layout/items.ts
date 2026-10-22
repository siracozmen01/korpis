// SPDX-License-Identifier: AGPL-3.0-or-later

// A layout's items as the editor changes them (question 2 of §10 of docs/11-surface-web.md):
// described in words, moved, removed, and added, each change a new array, so a draft is never
// changed in place. What the viewer's grants open is never removed, since the control plane
// refuses a layout without it; the quota is added at most once, for the same reason.

import { create } from "@bufbuild/protobuf";
import { type LayoutItem, LayoutItemSchema } from "../gen/korpis/v1/tenancy_pb";
import { t } from "../i18n/intl";

/** An item, in words. */
export function described(item: LayoutItem): string {
  switch (item.kind.case) {
    case "places":
      return t({ defaultMessage: "What the viewer can reach", id: "NwZtHmAF" });
    case "quota":
      return t({ defaultMessage: "The quota", id: "34EHh/pZ" });
    case "notice":
      return t(
        { defaultMessage: "A notice: {heading}", id: "EWcNOqcq" },
        {
          heading: item.kind.value.heading,
        },
      );
    case "link":
      return t(
        { defaultMessage: "A link: {label} ({url})", id: "BPVlDpFa" },
        {
          label: item.kind.value.label,
          url: item.kind.value.url,
        },
      );
    case "slot":
      return t({ defaultMessage: "The slot {name}", id: "js1JYn5k" }, { name: item.kind.value });
    default:
      return t({ defaultMessage: "An item this panel does not know", id: "W2AlNJQ/" });
  }
}

/** `items` with the one at `at` moved `by` places, where there is room. */
export function moved(items: readonly LayoutItem[], at: number, by: -1 | 1): LayoutItem[] {
  const to = at + by;
  const next = [...items];
  const [item] = next.splice(at, 1);
  if (item === undefined || to < 0 || to > items.length - 1) return [...items];
  next.splice(to, 0, item);
  return next;
}

/** `items` without the one at `at`, unless it is what the viewer's grants open. */
export function removed(items: readonly LayoutItem[], at: number): LayoutItem[] {
  return items.filter((item, i) => i !== at || item.kind.case === "places");
}

/** Whether `items` places the quota. */
export function hasQuota(items: readonly LayoutItem[]): boolean {
  return items.some((i) => i.kind.case === "quota");
}

/** A notice. */
export function notice(heading: string, text: string): LayoutItem {
  return create(LayoutItemSchema, { kind: { case: "notice", value: { heading, text } } });
}

/** A link. */
export function link(label: string, url: string): LayoutItem {
  return create(LayoutItemSchema, { kind: { case: "link", value: { label, url } } });
}

/** The quota. */
export function quota(): LayoutItem {
  return create(LayoutItemSchema, { kind: { case: "quota", value: {} } });
}

/** What the viewer's grants open. */
export function places(): LayoutItem {
  return create(LayoutItemSchema, { kind: { case: "places", value: {} } });
}
