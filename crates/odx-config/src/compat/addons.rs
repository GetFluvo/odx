use serde::Deserialize;
use serde::de::IgnoredAny;

use super::ordered::Ordered;
use crate::Problem;

/// A parsed doodba `addons.yaml`: repository name → addon globs.
///
/// Documents with an `ONLY` condition are skipped with a warning, because
/// they depend on the container environment; `ENV` defaults only feed those
/// conditions and are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AddonsFile {
    /// Repository name → addon globs, in file order.
    pub repos: Vec<(String, Vec<String>)>,
}

impl AddonsFile {
    /// The addon globs of repository `name`.
    pub fn addons(&self, name: &str) -> Option<&[String]> {
        self.repos
            .iter()
            .find(|(repo, _)| repo == name)
            .map(|(_, globs)| globs.as_slice())
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    One(String),
    Many(Vec<String>),
    Other(IgnoredAny),
}

pub(crate) fn parse(text: &str) -> Result<(AddonsFile, Vec<Problem>), String> {
    let docs: Vec<Ordered<Entry>> = serde_saphyr::from_multiple(text).map_err(|e| e.to_string())?;
    let mut file = AddonsFile::default();
    let mut warnings = Vec::new();
    for (i, doc) in docs.into_iter().enumerate() {
        if doc.0.iter().any(|(key, _)| key == "ONLY") {
            warnings.push(Problem {
                key: format!("document {}", i + 1),
                message: "has an ONLY condition; skipped (depends on the container environment)"
                    .to_owned(),
            });
            continue;
        }
        for (key, entry) in doc.0 {
            let globs = match entry {
                Entry::One(glob) => vec![glob],
                Entry::Many(globs) => globs,
                Entry::Other(_) if key == "ENV" => continue,
                Entry::Other(_) => {
                    warnings.push(Problem {
                        key,
                        message: "expected a list of addon names or globs; ignored".to_owned(),
                    });
                    continue;
                }
            };
            match file.repos.iter_mut().find(|(repo, _)| *repo == key) {
                Some((_, existing)) => existing.extend(globs),
                None => file.repos.push((key, globs)),
            }
        }
    }
    Ok((file, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_documents_and_skips_conditional_ones() {
        let text = r#"
web:
  - web_responsive
  - web_*
server-tools: "*"
ENV:
  DEFAULT_REPO_PATTERN: https://github.com/OCA/{}.git
---
web: [web_widget_x2many_2d_matrix]
---
ONLY:
  PGDATABASE: [prod]
pos: [pos_debug]
---
weird:
  nested: true
"#;
        let (file, warnings) = parse(text).unwrap();
        assert_eq!(
            file.addons("web").unwrap(),
            ["web_responsive", "web_*", "web_widget_x2many_2d_matrix"]
        );
        assert_eq!(file.addons("server-tools").unwrap(), ["*"]);
        assert_eq!(file.addons("pos"), None);
        let found: Vec<_> = warnings.iter().map(ToString::to_string).collect();
        assert_eq!(
            found,
            [
                "document 3: has an ONLY condition; skipped (depends on the container environment)",
                "weird: expected a list of addon names or globs; ignored",
            ]
        );
    }

    #[test]
    fn bad_yaml_is_an_error() {
        assert_ne!(parse("web: [\n").unwrap_err(), "");
    }
}
