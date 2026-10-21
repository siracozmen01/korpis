// SPDX-License-Identifier: AGPL-3.0-or-later

import { create } from "@bufbuild/protobuf";
import { UsageKind, UsageRecordSchema } from "../gen/korpis/v1/metering_pb";
import { cores, cpuSeconds, gibHours, permille, perSecond, totals } from "./figures";

const HOUR = 3600n;

function record(hour: bigint, kind: UsageKind, init: { cpuUsec?: bigint; diskBytes?: bigint }) {
  return create(UsageRecordSchema, {
    id: `use_${String(hour)}`,
    kind,
    from: { seconds: hour * HOUR },
    to: { seconds: (hour + 1n) * HOUR },
    ...init,
  });
}

test("a figure is shown in its unit", () => {
  expect(cpuSeconds(1_234_567n)).toBe("1.235 s");
  expect(gibHours(1024n * 1024n * 3600n * 3n)).toBe("3 GiB-hours");
  expect(cores(500n)).toBe("0.5 cores");
  expect(cores(1000n)).toBe("1 core");
  expect(permille(25n)).toBe("2.5%");
  expect(perSecond(2048n)).toBe("2 KiB/s");
});

test("a day's records sum what was measured and count the unheard apart", () => {
  const sum = totals([
    record(0n, UsageKind.MEASURED, { cpuUsec: 1_000_000n, diskBytes: 10n }),
    record(1n, UsageKind.GAP, {}),
    record(2n, UsageKind.ADJUSTMENT, { cpuUsec: 500_000n, diskBytes: 20n }),
    record(3n, UsageKind.MEASURED, {}),
  ]);
  expect(sum.cpuUsec).toBe(1_500_000n);
  expect(sum.measured).toBe(3);
  expect(sum.adjustments).toBe(1);
  expect(sum.gaps).toBe(1);
  expect(sum.gapMs).toBe(3_600_000);
  expect(sum.disk).toEqual({ bytes: 20n, at: 3 * 3_600_000 });
  expect(sum.netRxBytes, "nothing measured it: absent, not zero").toBeUndefined();
  expect(totals([]).cpuUsec).toBeUndefined();
});
