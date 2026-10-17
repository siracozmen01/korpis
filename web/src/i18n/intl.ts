// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's words (question 4 of §10 of docs/11-surface-web.md): every string it shows is a
// message with a stable identifier and its English text, written where it is used and extracted
// with its description into `en.json`, which a check holds to the source. The English is the
// source, so it needs no catalogue; `catalogues/<locale>.json` holds another locale's
// translations by identifier, and a check refuses one naming a message the source does not have
// or written in ICU that does not parse. The locale is the first of the browser's preferences a
// catalogue speaks, by its exact tag or its language alone, and English otherwise; a message a
// catalogue lacks, or holds in a form that cannot be read, is shown in English, never as its
// identifier. Every catalogue is in the bundle, so the locale is known before anything renders.

import { createIntl, createIntlCache, type IntlShape, type MessageDescriptor } from "react-intl";

/** A value a message's placeholder takes. */
export type Value = string | number | boolean | Date | null | undefined;

/** The source locale: the language the messages are written in. */
export const SOURCE = "en";

const loaded = import.meta.glob<Record<string, string>>("./catalogues/*.json", {
  eager: true,
  import: "default",
});

/** Each catalogue, by its locale's tag. */
const CATALOGUES: Readonly<Record<string, Record<string, string>>> = Object.fromEntries(
  Object.entries(loaded).map(([path, messages]) => [
    path.replace(/^.*\/([^/]+)\.json$/, "$1"),
    messages,
  ]),
);

/** The first of `asked` that one of `have` speaks, by its exact tag or by its language alone;
 * the source locale where none does. */
export function negotiate(asked: readonly string[], have: readonly string[]): string {
  const lower = new Map(have.map((h) => [h.toLowerCase(), h]));
  for (const tag of asked) {
    const exact = lower.get(tag.toLowerCase());
    if (exact !== undefined) return exact;
    const language = lower.get(tag.toLowerCase().split("-")[0] ?? "");
    if (language !== undefined) return language;
  }
  return SOURCE;
}

/** The panel's formatter in `locale`, speaking `messages`. */
export function intlFor(
  locale: string,
  messages: Readonly<Record<string, string>> = CATALOGUES[locale] ?? {},
): IntlShape {
  return createIntl(
    {
      locale,
      defaultLocale: SOURCE,
      messages,
      // A message a catalogue lacks or garbles is shown in English, which is what the error
      // says happened; there is nothing for the viewer to act on.
      onError: () => undefined,
    },
    createIntlCache(),
  );
}

const asked = typeof navigator === "undefined" ? [] : navigator.languages;

/** The formatter the panel speaks with, in the browser's locale. */
export const intl: IntlShape = intlFor(negotiate(asked, [SOURCE, ...Object.keys(CATALOGUES)]));

/** `message` in the panel's locale, its placeholders filled from `values`. */
export function t(message: MessageDescriptor, values?: Readonly<Record<string, Value>>): string {
  return intl.formatMessage(message, values);
}
