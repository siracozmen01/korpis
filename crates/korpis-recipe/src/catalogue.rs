// SPDX-License-Identifier: AGPL-3.0-or-later

//! A recipe's catalogues (§5.3 of docs/09-recipes.md): files `i18n/<locale>.json` of its
//! artifact, each a JSON object from a message's key to its translation. They are read with the
//! document when the recipe is resolved, bounded in size and number, and checked against the
//! recipe they came in, so a catalogue that names a message the recipe does not show, or holds
//! one the panel could not show as meant, keeps the recipe from loading.

use crate::document::DocumentError;
use korpis_core::recipe::messages::Catalogue;
use korpis_core::recipe::{Recipe, RecipeError};
use std::collections::BTreeMap;

/// The directory of the artifact that holds catalogues.
pub const DIRECTORY: &str = "i18n/";

/// The largest catalogue accepted, in bytes.
pub const MAX_CATALOGUE: usize = 256 * 1024;

/// The most catalogues a recipe may carry.
pub const MAX_CATALOGUES: usize = 200;

/// The locale a file titled `title` is the catalogue of, if it is one.
#[must_use]
pub fn locale_of(title: &str) -> Option<&str> {
    title.strip_prefix(DIRECTORY)?.strip_suffix(".json").filter(|l| !l.contains('/'))
}

/// The catalogues `raw` holds, by locale, each parsed and checked against `recipe`.
pub fn read(
    recipe: &Recipe,
    raw: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, Catalogue>, DocumentError> {
    if raw.len() > MAX_CATALOGUES {
        let why = format!("at most {MAX_CATALOGUES} catalogues");
        return Err(RecipeError::new(DIRECTORY, why).into());
    }
    raw.iter()
        .map(|(locale, bytes)| {
            let at = format!("{DIRECTORY}{locale}.json");
            if bytes.len() > MAX_CATALOGUE {
                return Err(RecipeError::new(&at, format!("at most {MAX_CATALOGUE} bytes")).into());
            }
            let catalogue: Catalogue = serde_json::from_slice(bytes).map_err(|e| {
                DocumentError::Syntax(format!("{at}: an object of message keys to strings: {e}"))
            })?;
            recipe.check_catalogue(locale, &catalogue)?;
            Ok((locale.clone(), catalogue))
        })
        .collect()
}

/// Checks catalogues already read, as the store kept them: the rules may have grown since.
pub fn check(
    recipe: &Recipe,
    catalogues: &BTreeMap<String, Catalogue>,
) -> Result<(), DocumentError> {
    if catalogues.len() > MAX_CATALOGUES {
        let why = format!("at most {MAX_CATALOGUES} catalogues");
        return Err(RecipeError::new(DIRECTORY, why).into());
    }
    for (locale, catalogue) in catalogues {
        recipe.check_catalogue(locale, catalogue)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{locale_of, read};
    use crate::document::parse;
    use std::collections::BTreeMap;

    const DOCUMENT: &str = r#"
apiVersion: korpis.dev/v1
kind: Recipe
metadata:
  name: paper
  version: "1"
  description: "A Minecraft server"
runtime:
  targets:
    - tier: container
      arch: [amd64]
      image: docker.io/library/temurin@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
config:
  fields:
    - key: max_players
      type: integer
      label: "Maximum players"
      permission: tenant
"#;

    fn raw(locale: &str, json: &str) -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([(locale.to_owned(), json.as_bytes().to_vec())])
    }

    #[test]
    fn a_catalogue_translates_only_what_the_recipe_shows() {
        let recipe = parse(DOCUMENT.as_bytes()).unwrap();
        assert_eq!(recipe.metadata.locale, "en", "English unless it says");
        let tr = r#"{"config.fields.max_players.label": "En fazla oyuncu",
                     "metadata.description": "Bir Minecraft sunucusu"}"#;
        let read_tr = read(&recipe, &raw("tr", tr)).unwrap();
        assert_eq!(read_tr["tr"]["config.fields.max_players.label"], "En fazla oyuncu");
        let cases = [
            ("tr", r#"{"config.fields.motd.label": "x"}"#, "not a message the recipe shows"),
            ("tr", r#"{"metadata.description": "{name}"}"#, "no placeholder"),
            ("tr", r#"{"metadata.description": 3}"#, "an object of message keys"),
            ("en", r#"{"metadata.description": "x"}"#, "the default locale"),
            ("tr_TR", "{}", "not a language tag"),
        ];
        for (locale, json, said) in cases {
            let e = read(&recipe, &raw(locale, json)).unwrap_err().to_string();
            assert!(e.contains(said), "{locale} {json}: {e}");
        }
    }

    #[test]
    fn a_catalogue_is_a_json_file_in_i18n() {
        assert_eq!(locale_of("i18n/pt-BR.json"), Some("pt-BR"));
        assert_eq!(locale_of("i18n/x/tr.json"), None);
        assert_eq!(locale_of("templates/tr.json"), None);
        assert_eq!(locale_of("i18n/tr.yaml"), None);
    }
}
