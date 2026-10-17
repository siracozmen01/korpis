// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel while the control plane cannot be reached (§2 and question 3 of §10 of
// docs/11-surface-web.md): it may have loaded from the browser's copy of the bundle, but nothing
// the control plane holds was kept, so every value is unknown and the page says so, with how long
// this browser has not heard from it. It asks again on its own, and when told to.

import { useEffect } from "react";
import { Button } from "react-aria-components";
import { t } from "../i18n/intl";
import { age } from "../workload/freshness";
import { useNow } from "../workload/useNow";
import { lastHeard } from "./heard";

/** How often the page asks again by itself. */
const AGAIN_MS = 15_000;

/** The page while the control plane cannot be reached; `retry` asks again. */
export function Unreachable({ retry }: { readonly retry: () => void }) {
  const now = useNow();
  useEffect(() => {
    const timer = setInterval(retry, AGAIN_MS);
    return () => {
      clearInterval(timer);
    };
  }, [retry]);
  const at = lastHeard();
  return (
    <main>
      <p role="alert">
        {t({ defaultMessage: "The control plane cannot be reached.", id: "YVga+2AS" })}
      </p>
      <p>
        {t({
          defaultMessage:
            "Every value it holds is unknown until it answers: nothing it said was kept in this browser.",
          id: "NM6+o6pb",
        })}
      </p>
      <p>
        {at === undefined
          ? t({ defaultMessage: "This browser has not heard from it before.", id: "pSiQGWZY" })
          : t(
              { defaultMessage: "This browser last heard from it {ago} ago.", id: "dastCq+7" },
              { ago: age(now - at) },
            )}
      </p>
      <Button onPress={retry}>{t({ defaultMessage: "Try again", id: "FazwRldA" })}</Button>
    </main>
  );
}
