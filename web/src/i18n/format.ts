// SPDX-License-Identifier: AGPL-3.0-or-later

// The figures the panel shows, in its locale: a byte count in binary units, each unit's symbol a
// message so a language that writes "Kio" for "KiB" can, and its number grouped and pointed as
// the locale does.

import { intl, t } from "./intl";

const UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"] as const;

/** A byte count as a person reads it: the largest binary unit it reaches, to a tenth. */
export function bytes(n: number | bigint): string {
  let v = Number(n);
  let u = 0;
  while (v >= 1024 && u < UNITS.length - 1) {
    v /= 1024;
    u += 1;
  }
  const size = intl.formatNumber(v, { maximumFractionDigits: u === 0 ? 0 : 1 });
  return t(
    {
      defaultMessage:
        "{unit, select, B {{size} B} KiB {{size} KiB} MiB {{size} MiB} GiB {{size} GiB} TiB {{size} TiB} other {{size} PiB}}",
      id: "kb5FY9Br",
      description: "a byte count in binary units",
    },
    { unit: UNITS[u], size },
  );
}
