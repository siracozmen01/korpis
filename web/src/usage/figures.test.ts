// SPDX-License-Identifier: AGPL-3.0-or-later

import { create } from "@bufbuild/protobuf";
import { UsageKind, UsageRecordSchema } from "../gen/korpis/v1/metering_pb";
import { DeviceClass } from "../gen/korpis/v1/common_pb";
import {
  coreHours,
  cores,
  cpuSeconds,
  deviceHours,
  gibHours,
  permille,
  perSecond,
  totals,
} from "./figures";

const HOUR = 3600n;

interface Init {
  readonly cpuUsec?: bigint;
  readonly diskBytes?: bigint;
  readonly diskAllocated?: boolean;
  readonly egressBytes?: bigint;
  readonly endpoints?: { endpoint: string; receivedBytes: bigint; sentBytes: bigint }[];
  readonly dedicatedCoreSeconds?: bigint;
  readonly deviceSeconds?: { class: DeviceClass; seconds: bigint }[];
}

function record(hour: bigint, kind: UsageKind, init: Init) {
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
  expect(sum.disk).toEqual({ bytes: 20n, at: 3 * 3_600_000, allocated: false });
  expect(sum.netRxBytes, "nothing measured it: absent, not zero").toBeUndefined();
  expect(totals([]).cpuUsec).toBeUndefined();
});

test("a microVM's disk is the latest record's, and said to be what its devices allocated", () => {
  const sum = totals([
    record(0n, UsageKind.MEASURED, { diskBytes: 10n }),
    record(1n, UsageKind.MEASURED, { diskBytes: 30n, diskAllocated: true }),
  ]);
  expect(sum.disk).toEqual({ bytes: 30n, at: 2 * 3_600_000, allocated: true });
});

test("a day's egress and each counted endpoint are summed by name", () => {
  const game = (receivedBytes: bigint, sentBytes: bigint) => ({
    endpoint: "game",
    receivedBytes,
    sentBytes,
  });
  const sum = totals([
    record(0n, UsageKind.MEASURED, { egressBytes: 900n, endpoints: [game(100n, 400n)] }),
    record(1n, UsageKind.MEASURED, { egressBytes: 100n, endpoints: [game(20n, 30n)] }),
  ]);
  expect(sum.egressBytes).toBe(1000n);
  expect(sum.endpoints.get("game")).toEqual({ receivedBytes: 120n, sentBytes: 430n });
  expect(totals([record(2n, UsageKind.MEASURED, {})]).endpoints.size, "none counted").toBe(0);
});

test("held cores and devices are summed, each class apart, and absent where none was", () => {
  const gpu = (seconds: bigint) => [{ class: DeviceClass.GPU, seconds }];
  const sum = totals([
    record(0n, UsageKind.MEASURED, { dedicatedCoreSeconds: 4n * HOUR, deviceSeconds: gpu(HOUR) }),
    record(1n, UsageKind.MEASURED, { dedicatedCoreSeconds: 4n * HOUR, deviceSeconds: gpu(HOUR) }),
  ]);
  expect(coreHours(sum.dedicatedCoreSeconds ?? 0n)).toBe("8 core-hours");
  expect(deviceHours(sum.deviceSeconds)).toBe("GPU 2 hours");
  const none = totals([record(2n, UsageKind.MEASURED, {})]);
  expect(none.dedicatedCoreSeconds, "nothing held, nothing shown").toBeUndefined();
  expect(none.deviceSeconds.size).toBe(0);
});
