// SPDX-License-Identifier: AGPL-3.0-or-later

// The landing screen (question 2 of §10 of docs/11-surface-web.md): the layout an organization
// composed for those it serves, its items in order, each read with the viewer's own credential,
// so a host's landing shown to a customer shows the customer's view; with no layout, the
// panel's own, what the viewer's grants open. A notice is shown as text and a link with the host
// it goes to, since the panel does not vouch for either. A layout that cannot be read leaves the
// panel's own landing, which is what its absence means.

import { useCallback } from "react";
import { useReach } from "../clients";
import type { LayoutItem, Organization } from "../gen/korpis/v1/tenancy_pb";
import { t } from "../i18n/intl";
import { useLoad } from "../views/load";
import { Home } from "../views/Places";
import { QuotaView } from "../views/Quota";
import { hostOf, keyOf, landingOf } from "./landing";

/** One item of a layout, as the viewer sees it. */
function Item(props: {
  readonly item: LayoutItem;
  readonly at: number;
  readonly top: Organization | undefined;
}) {
  const { item, at, top } = props;
  switch (item.kind.case) {
    case "places":
      return <Home />;
    case "quota":
      return top === undefined ? null : <QuotaView organization={top} />;
    case "notice": {
      const id = `notice-${String(at)}`;
      return (
        <section aria-labelledby={id} className="notice">
          <h2 id={id}>{item.kind.value.heading}</h2>
          <p className="kept-lines">{item.kind.value.text}</p>
        </section>
      );
    }
    case "link": {
      const { label, url } = item.kind.value;
      const host = hostOf(url);
      if (host === undefined) return null;
      return (
        <p>
          <a href={url} rel="noopener noreferrer" target="_blank">
            {label}
          </a>
          <small className="aside">
            {t({ defaultMessage: "(goes to {host})", id: "luUrKvb9" }, { host })}
          </small>
        </p>
      );
    }
    default:
      // A slot, or a kind this panel does not know: nothing it could render under its own rules.
      return null;
  }
}

/** The landing screen of the signed-in viewer. */
export function Landing() {
  const { clients } = useReach();
  const loaded = useLoad(useCallback(async () => landingOf(clients), [clients]));
  if (loaded.state === "loading") {
    return <p>{t({ defaultMessage: "Reading your landing screen…", id: "8aIROWWx" })}</p>;
  }
  if (loaded.state !== "done" || loaded.value.layout === undefined) return <Home />;
  const { layout, top } = loaded.value;
  return (
    <>
      {layout.name !== "" && <p className="landing-name">{layout.name}</p>}
      {layout.items.map((item, at) => (
        <Item key={keyOf(item, at)} item={item} at={at} top={top} />
      ))}
    </>
  );
}
