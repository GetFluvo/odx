//! `odx promote --search`: addons and feature sets ranked by how well they
//! match a functional description.

use std::collections::{BTreeMap, BTreeSet};

/// Words of one Odoo domain that mean the same thing in a search.
const SYNONYMS: &[&[&str]] = &[
    &[
        "invoice",
        "invoicing",
        "billing",
        "bill",
        "account",
        "accounting",
    ],
    &["stock", "inventory", "warehouse", "picking", "delivery"],
    &["sale", "sales", "selling", "quotation", "order"],
    &[
        "purchase",
        "purchasing",
        "procurement",
        "vendor",
        "supplier",
    ],
    &["pos", "point", "register", "cashier"],
    &["hr", "employee", "employees", "staff"],
    &["crm", "lead", "leads", "opportunity", "pipeline"],
    &["mrp", "manufacturing", "production", "bom"],
    &["website", "ecommerce", "shop", "webshop"],
    &["helpdesk", "ticket", "tickets", "support"],
    &["timesheet", "timesheets", "hours"],
    &["payment", "payments", "pay"],
    &["partner", "contact", "contacts", "customer", "customers"],
    &["report", "reports", "reporting"],
];

/// Words that say nothing about what is meant.
const STOP: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "for", "feature", "module", "modules", "addon",
];

/// Something `--search` can find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// A feature set (its modules are the selection) rather than an addon.
    pub feature: bool,
    /// Addon or feature name.
    pub name: String,
    /// Strongest text first: the technical name, manifest `name`,
    /// `summary`, then `category` and `description`.
    pub texts: Vec<String>,
}

/// A candidate's score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The candidate's index.
    pub index: usize,
    /// Higher is better.
    pub score: u32,
}

/// `word` lowercased, without a plural `s`.
fn stem(word: &str) -> String {
    let word = word.to_lowercase();
    match word.strip_suffix('s') {
        Some(single) if single.len() > 2 && !single.ends_with('s') => single.to_owned(),
        _ => word,
    }
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty() && !STOP.contains(&word.to_lowercase().as_str()))
        .map(stem)
        .collect()
}

/// The words `word` stands for: itself and its synonyms.
fn meanings(word: &str, extra: &BTreeMap<String, Vec<String>>) -> BTreeSet<String> {
    let mut found = BTreeSet::from([word.to_owned()]);
    for (key, values) in extra {
        let key = stem(key);
        let values: Vec<String> = values.iter().map(|value| stem(value)).collect();
        if key == word || values.iter().any(|value| value == word) {
            found.insert(key);
            found.extend(values);
        }
    }
    let project: Vec<String> = found.iter().cloned().collect();
    for group in SYNONYMS {
        if group.iter().any(|w| project.contains(&stem(w))) {
            found.extend(group.iter().map(|w| stem(w)));
        }
    }
    found
}

/// The candidates that match `query`, best first.
///
/// An exact feature name ranks above everything, then the words matched
/// count (its own words above synonyms), weighted by the text they are in.
/// `extra` adds synonyms (`[tool.odx.promote] search_synonyms`).
#[must_use]
pub fn search(
    query: &str,
    candidates: &[Candidate],
    extra: &BTreeMap<String, Vec<String>>,
) -> Vec<Hit> {
    let wanted: Vec<(String, BTreeSet<String>)> = words(query)
        .into_iter()
        .map(|word| {
            let meant = meanings(&word, extra);
            (word, meant)
        })
        .collect();
    let exact = words(query);
    let mut hits: Vec<Hit> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let mut score = 0;
            if candidate.feature && words(&candidate.name) == exact {
                score += 1000;
            }
            for (position, text) in candidate.texts.iter().enumerate() {
                let weight = match position {
                    0 => 4,
                    1 => 3,
                    2 => 2,
                    _ => 1,
                };
                let found = words(text);
                for (word, meant) in &wanted {
                    if found.contains(word) {
                        score += 3 * weight;
                    } else if found.iter().any(|w| meant.contains(w)) {
                        score += weight;
                    }
                }
            }
            (score > 0).then_some(Hit { index, score })
        })
        .collect();
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(feature: bool, name: &str, texts: &[&str]) -> Candidate {
        Candidate {
            feature,
            name: name.to_owned(),
            texts: texts.iter().map(|text| (*text).to_owned()).collect(),
        }
    }

    #[test]
    fn ranking() {
        let candidates = [
            candidate(
                false,
                "stock_x",
                &["stock_x", "Stock extras", "", "Inventory", "Moves"],
            ),
            candidate(
                false,
                "account_due",
                &["account_due", "Due dates", "On invoices"],
            ),
            candidate(true, "invoicing", &["invoicing", "Invoices sent by mail"]),
            candidate(false, "account_tax", &["account_tax", "Taxes"]),
            candidate(false, "website_y", &["website_y", "Shop"]),
            candidate(true, "billing extras", &["billing extras", "Credit notes"]),
        ];
        let none = BTreeMap::new();
        let ranked: Vec<(&str, u32)> = search("the invoicing feature", &candidates, &none)
            .iter()
            .map(|hit| (candidates[hit.index].name.as_str(), hit.score))
            .collect();
        assert_eq!(
            ranked,
            [
                ("invoicing", 1015),
                ("account_due", 6),
                ("account_tax", 4),
                ("billing extras", 4),
            ]
        );
        // Project synonyms join words, in either direction.
        let extra = BTreeMap::from([("webshop".to_owned(), vec!["Eshop".to_owned()])]);
        let found = search("eshop", &candidates, &extra);
        assert_eq!(found.len(), 1);
        assert_eq!(candidates[found[0].index].name, "website_y");
        assert_eq!(search("webshop", &candidates, &extra).len(), 1);
        assert_eq!(search("invoicing", &candidates, &extra).len(), 4);
        assert_eq!(search("of the", &candidates, &none), []);
    }
}
