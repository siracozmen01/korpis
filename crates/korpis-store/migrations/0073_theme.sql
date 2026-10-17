-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Each organization's theme (§7 of docs/11-surface-web.md). An organization without one wears the
-- nearest ancestor's, and with none anywhere the panel's own. Every field is bounded by the
-- application before it is written; the checks here are the same bounds, so a row the
-- application did not check cannot be stored either. An organization that removes its theme
-- keeps its row, marked `inherit`, so the version keeps counting and a client holding an old
-- version cannot overwrite a newer theme.

CREATE TABLE organization_theme (
    organization_id  uuid PRIMARY KEY REFERENCES organization (id),
    product_name     text NOT NULL DEFAULT '' CHECK (char_length(product_name) <= 64),
    logo_media_type  text CHECK (logo_media_type IN ('image/png', 'image/webp', 'image/svg+xml')),
    logo             bytea CHECK (octet_length(logo) <= 262144),
    light            jsonb NOT NULL DEFAULT '{}',
    dark             jsonb NOT NULL DEFAULT '{}',
    sign_in_text     text NOT NULL DEFAULT '' CHECK (char_length(sign_in_text) <= 2000),
    stylesheet       text NOT NULL DEFAULT '' CHECK (octet_length(stylesheet) <= 65536),
    inherit          boolean NOT NULL DEFAULT false,
    version          bigint NOT NULL CHECK (version > 0),
    set_by           text NOT NULL,
    set_at           timestamptz NOT NULL,
    CHECK ((logo IS NULL) = (logo_media_type IS NULL))
);

GRANT SELECT, INSERT, UPDATE ON organization_theme TO korpis_app;
