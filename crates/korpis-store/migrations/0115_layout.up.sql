-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Each organization's layout (question 2 of §10 of docs/11-surface-web.md): the landing screen it
-- composes for those it serves. An organization without one shows the nearest ancestor's, and
-- with none anywhere the panel's own. Its items are an array of the panel's components, in order,
-- each bounded by the application before it is written and checked again when read; the checks
-- here are the bounds a row can be held to without reading inside its items. As for a theme, an
-- organization that removes its layout keeps its row, marked `inherit`, so the version keeps
-- counting and a client holding an old version cannot overwrite a newer layout.

CREATE TABLE organization_layout (
    organization_id  uuid PRIMARY KEY REFERENCES organization (id),
    name             text NOT NULL DEFAULT '' CHECK (char_length(name) <= 64),
    items            jsonb NOT NULL DEFAULT '[]'
                     CHECK (jsonb_typeof(items) = 'array' AND jsonb_array_length(items) <= 24),
    inherit          boolean NOT NULL DEFAULT false,
    version          bigint NOT NULL CHECK (version > 0),
    set_by           text NOT NULL,
    set_at           timestamptz NOT NULL
);

GRANT SELECT, INSERT, UPDATE ON organization_layout TO korpis_app;
