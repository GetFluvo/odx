//! The addons Odoo itself ships (community edition), per series.
//!
//! The lists are data generated from the `odoo/odoo` repository: every
//! directory with a `__manifest__.py` under `addons/` and `odoo/addons/`
//! on the series branch.

/// The core addons of `series`, sorted; `None` for a series without a
/// bundled list.
///
/// ```
/// let core = odx_analyze::core::core_addons("17.0").unwrap();
/// assert!(core.contains(&"sale"));
/// assert_eq!(odx_analyze::core::core_addons("8.0"), None);
/// ```
#[must_use]
pub fn core_addons(series: &str) -> Option<Vec<&'static str>> {
    let list = match series {
        "16.0" => include_str!("../data/core/16.0.txt"),
        "17.0" => include_str!("../data/core/17.0.txt"),
        "18.0" => include_str!("../data/core/18.0.txt"),
        "19.0" => include_str!("../data/core/19.0.txt"),
        _ => return None,
    };
    Some(list.lines().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_series_has_base_and_web() {
        for series in ["16.0", "17.0", "18.0", "19.0"] {
            let core = core_addons(series).unwrap();
            assert!(core.contains(&"base") && core.contains(&"web"), "{series}");
            assert!(core.windows(2).all(|pair| pair[0] < pair[1]), "{series}");
        }
        assert_eq!(core_addons("15.0"), None);
    }
}
