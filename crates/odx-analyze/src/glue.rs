//! Glue addons, which Odoo installs on its own (`auto_install`).
//!
//! Odoo installs a glue addon once the addons it joins are installed. A
//! project that adds whole repositories gets them along; one that names its
//! addons one by one does not, and nothing depends on them to pull them in.

use std::collections::{BTreeMap, BTreeSet};

/// An addon with `auto_install` that a repository offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offered {
    /// The addon.
    pub addon: String,
    /// Its `depends`.
    pub depends: Vec<String>,
    /// The addons whose installation installs it.
    pub triggers: Vec<String>,
    /// The repository URL.
    pub url: String,
    /// The addon's folder in the repository.
    pub path: String,
}

/// A glue addon Odoo would install with what the project installs, if the
/// project had it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The addon, where it is and what it needs.
    pub offered: Offered,
    /// Its `depends` the project does not have either.
    pub missing: Vec<String>,
}

/// The addons installed with `roots`: them and everything they depend on,
/// as far as `depends` (addon to its `depends`) knows.
pub fn installed<'a>(
    roots: impl IntoIterator<Item = &'a str>,
    depends: &BTreeMap<String, Vec<String>>,
) -> BTreeSet<String> {
    let mut installed = BTreeSet::new();
    let mut todo: Vec<&str> = roots.into_iter().collect();
    while let Some(addon) = todo.pop() {
        if installed.insert(addon.to_owned()) {
            todo.extend(depends.get(addon).into_iter().flatten().map(String::as_str));
        }
    }
    installed
}

/// The glue addons among `offered` that would be installed with
/// `installed` but are not `present`.
///
/// Every trigger is installed, and at least one trigger is not Odoo's own
/// (in `core`), so that the glue joins something the project brings in.
/// The first offer of an addon wins.
pub fn candidates(
    offered: &[Offered],
    installed: &BTreeSet<String>,
    present: &BTreeSet<String>,
    core: &BTreeSet<&str>,
) -> Vec<Candidate> {
    let mut seen = BTreeSet::new();
    offered
        .iter()
        .filter(|offer| seen.insert(offer.addon.as_str()))
        .filter(|offer| {
            !present.contains(&offer.addon)
                && !core.contains(offer.addon.as_str())
                && !offer.triggers.is_empty()
                && offer.triggers.iter().all(|t| installed.contains(t))
                && offer.triggers.iter().any(|t| !core.contains(t.as_str()))
        })
        .map(|offer| Candidate {
            missing: offer
                .depends
                .iter()
                .filter(|dep| !present.contains(*dep) && !core.contains(dep.as_str()))
                .cloned()
                .collect(),
            offered: offer.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(addon: &str, depends: &[&str], triggers: &[&str]) -> Offered {
        Offered {
            addon: addon.to_owned(),
            depends: depends.iter().map(|d| (*d).to_owned()).collect(),
            triggers: triggers.iter().map(|d| (*d).to_owned()).collect(),
            url: "https://github.com/OCA/pos.git".to_owned(),
            path: addon.to_owned(),
        }
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn installed_follows_dependencies() {
        let depends = BTreeMap::from([
            ("pos_x".to_owned(), vec!["point_of_sale".to_owned()]),
            (
                "point_of_sale".to_owned(),
                vec!["stock".to_owned(), "pos_x".to_owned()],
            ),
        ]);
        assert_eq!(
            installed(["pos_x"], &depends),
            set(&["point_of_sale", "pos_x", "stock"])
        );
    }

    #[test]
    fn glue_joins_what_the_project_installs() {
        let core = BTreeSet::from(["sale", "point_of_sale", "sale_stock"]);
        let offered = [
            // Joins an added addon with Odoo's sale: a candidate, that also
            // needs another addon.
            offer(
                "pos_x_sale",
                &["pos_x", "sale", "pos_y"],
                &["pos_x", "sale"],
            ),
            // A second offer of the same addon is ignored.
            offer("pos_x_sale", &[], &["sale"]),
            // Already there.
            offer("pos_z_sale", &["pos_z", "sale"], &["pos_z", "sale"]),
            // Not every trigger is installed.
            offer("pos_x_web", &["pos_x", "web_q"], &["pos_x", "web_q"]),
            // Only Odoo's own addons trigger it.
            offer(
                "sale_pos_glue",
                &["sale", "point_of_sale"],
                &["sale", "point_of_sale"],
            ),
            // Odoo's own glue.
            offer("sale_stock", &["sale"], &["pos_x"]),
            // Nothing triggers it.
            offer("lonely", &[], &[]),
        ];
        let installed = set(&["pos_x", "pos_z", "sale", "point_of_sale"]);
        let present = set(&["pos_x", "pos_z", "pos_z_sale"]);
        let found = candidates(&offered, &installed, &present, &core);
        assert_eq!(
            found,
            [Candidate {
                offered: offered[0].clone(),
                missing: vec!["pos_y".to_owned()],
            }]
        );
    }
}
