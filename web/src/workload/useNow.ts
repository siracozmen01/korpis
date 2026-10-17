// SPDX-License-Identifier: AGPL-3.0-or-later

// The time, rendered again each second, so an age on screen grows while it is read rather than
// standing still at what it was when the screen last changed.

import { useEffect, useState } from "react";

/** Now, in milliseconds, renewed every `everyMs`. */
export function useNow(everyMs = 1000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => {
      setNow(Date.now());
    }, everyMs);
    return () => {
      clearInterval(timer);
    };
  }, [everyMs]);
  return now;
}
