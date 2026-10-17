// SPDX-License-Identifier: AGPL-3.0-or-later

import { retrying } from "./retrying";

const answer = new Response("ok");

test("a request the network failed is sent again until it is answered", async () => {
  const inner = vi
    .fn<typeof fetch>()
    .mockRejectedValueOnce(new TypeError("Failed to fetch"))
    .mockRejectedValueOnce(new TypeError("Failed to fetch"))
    .mockResolvedValueOnce(answer);
  await expect(retrying(inner, [0, 0])("/a", { method: "POST" })).resolves.toBe(answer);
  expect(inner).toHaveBeenCalledTimes(3);
});

test("it is sent at most three times, and the last failure is the one told", async () => {
  const inner = vi.fn<typeof fetch>().mockRejectedValue(new TypeError("Failed to fetch"));
  await expect(retrying(inner, [0, 0])("/a")).rejects.toThrow("Failed to fetch");
  expect(inner).toHaveBeenCalledTimes(3);
});

test("a request asked to stop is not sent again", async () => {
  const stop = new AbortController();
  const inner = vi.fn<typeof fetch>().mockImplementation(() => {
    stop.abort();
    return Promise.reject(new DOMException("aborted", "AbortError"));
  });
  await expect(retrying(inner, [0])("/a", { signal: stop.signal })).rejects.toThrow("aborted");
  expect(inner).toHaveBeenCalledTimes(1);
});

test("a request stopped while it waits is not sent again", async () => {
  const stop = new AbortController();
  const inner = vi.fn<typeof fetch>().mockImplementation(() => {
    setTimeout(() => {
      stop.abort(new Error("left the screen"));
    }, 0);
    return Promise.reject(new TypeError("Failed to fetch"));
  });
  await expect(retrying(inner, [60_000])("/a", { signal: stop.signal })).rejects.toThrow(
    "left the screen",
  );
  expect(inner).toHaveBeenCalledTimes(1);
});

test("a Request, whose body reads once, is sent once", async () => {
  const inner = vi.fn<typeof fetch>().mockRejectedValue(new TypeError("Failed to fetch"));
  await expect(retrying(inner, [0])(new Request("http://x/a"))).rejects.toThrow();
  expect(inner).toHaveBeenCalledTimes(1);
});
