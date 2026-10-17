// SPDX-License-Identifier: AGPL-3.0-or-later

// An authenticator app's address as a QR code, drawn in the browser from the address itself, so
// the TOTP secret it carries never leaves the page for an image service. The address is shown
// as text beside it too, for an app that cannot scan.

import { renderSVG } from "uqr";

/** The `otpauth://` address `uri`, as a QR code and as text. */
export function Qr({ uri }: { readonly uri: string }) {
  return (
    <>
      <img
        alt="QR code of the authenticator address"
        src={`data:image/svg+xml;utf8,${encodeURIComponent(renderSVG(uri))}`}
      />
      <p>
        <code>{uri}</code>
      </p>
    </>
  );
}
