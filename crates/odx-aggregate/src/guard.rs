use std::time::{SystemTime, UNIX_EPOCH};

use odx_config::lock::{CiState, RedClassification};
use odx_config::schema::OnHeadChange;

/// What to do with a pull request whose head moved since it was locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadDecision {
    /// Take the new head.
    Adopt,
    /// Keep the locked commit.
    Keep,
}

/// The force-push guard rail: whether to adopt a moved head.
///
/// The policy decides with the CI state of the new head. `follow` (from
/// `--follow <repo>#<pr>`) adopts deliberately. A head whose forge reports
/// no checks at all is adopted under `follow-if-green`: there is nothing
/// to wait for.
///
/// ```
/// use odx_aggregate::{HeadDecision, decide};
/// use odx_config::lock::CiState;
/// use odx_config::schema::OnHeadChange;
///
/// assert_eq!(decide(OnHeadChange::FollowIfGreen, CiState::Red, false), HeadDecision::Keep);
/// assert_eq!(decide(OnHeadChange::FollowIfGreen, CiState::Red, true), HeadDecision::Adopt);
/// ```
pub fn decide(policy: OnHeadChange, ci: CiState, follow: bool) -> HeadDecision {
    match (follow, policy, ci) {
        (true, _, _)
        | (false, OnHeadChange::Follow, _)
        | (false, OnHeadChange::FollowIfGreen, CiState::Green | CiState::Unknown) => {
            HeadDecision::Adopt
        }
        (false, OnHeadChange::FollowIfGreen, CiState::Pending | CiState::Red)
        | (false, OnHeadChange::Pin, _) => HeadDecision::Keep,
    }
}

/// Why a head is red, as far as can be told: the base branch fails the
/// same checks, the failing checks name only addons the pull request does
/// not touch, or neither (plain red).
///
/// ```
/// use odx_aggregate::classify;
/// use odx_config::lock::RedClassification;
///
/// let failing = ["test pos_vouchers".to_owned()];
/// let changed = ["pos_loyalty".to_owned()];
/// let addons = ["pos_loyalty".to_owned(), "pos_vouchers".to_owned()];
/// assert_eq!(classify(&failing, None, &changed, &addons), RedClassification::OtherAddon);
/// ```
pub fn classify(
    failing: &[String],
    base_failing: Option<&[String]>,
    changed_addons: &[String],
    addons: &[String],
) -> RedClassification {
    if base_failing.is_some_and(|base| failing.iter().any(|check| base.contains(check))) {
        return RedClassification::BaseAlsoFailing;
    }
    let mentioned = |check: &String| -> Vec<&String> {
        addons
            .iter()
            .filter(|addon| check.contains(addon.as_str()))
            .collect()
    };
    let elsewhere = !failing.is_empty()
        && failing.iter().all(|check| {
            let addons_named = mentioned(check);
            !addons_named.is_empty()
                && addons_named
                    .iter()
                    .all(|addon| !changed_addons.contains(addon))
        });
    if elsewhere {
        RedClassification::OtherAddon
    } else {
        RedClassification::Red
    }
}

/// `YYYY-MM-DD` (UTC) of `time`.
///
/// ```
/// use odx_aggregate::date;
/// use std::time::{Duration, UNIX_EPOCH};
///
/// assert_eq!(date(UNIX_EPOCH + Duration::from_secs(1_760_000_000)), "2025-10-09");
/// ```
pub fn date(time: SystemTime) -> String {
    let days = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() / 86_400);
    // Days to civil date (Howard Hinnant's algorithm), for dates after 1970.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn every_policy_cell() {
        use CiState::{Green, Pending, Red, Unknown};
        use HeadDecision::{Adopt, Keep};
        for (policy, expected) in [
            (OnHeadChange::Follow, [Adopt, Adopt, Adopt, Adopt]),
            (OnHeadChange::FollowIfGreen, [Adopt, Keep, Keep, Adopt]),
            (OnHeadChange::Pin, [Keep, Keep, Keep, Keep]),
        ] {
            let decided = [Green, Pending, Red, Unknown].map(|ci| decide(policy, ci, false));
            assert_eq!(decided, expected, "{policy:?}");
            assert_eq!(
                [Green, Red].map(|ci| decide(policy, ci, true)),
                [Adopt, Adopt]
            );
        }
    }

    #[test]
    fn classifications() {
        let s = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        let addons = s(&["pos_loyalty", "pos_vouchers"]);
        let changed = s(&["pos_loyalty"]);
        let base = s(&["test with Odoo"]);
        assert_eq!(
            classify(&s(&["test with Odoo"]), Some(&base), &changed, &addons),
            RedClassification::BaseAlsoFailing
        );
        assert_eq!(
            classify(&s(&["lint"]), Some(&base), &changed, &addons),
            RedClassification::Red
        );
        assert_eq!(
            classify(&s(&["pos_vouchers tests"]), None, &changed, &addons),
            RedClassification::OtherAddon
        );
        assert_eq!(
            classify(&s(&["pos_loyalty tests"]), None, &changed, &addons),
            RedClassification::Red
        );
        assert_eq!(
            classify(&[], None, &changed, &addons),
            RedClassification::Red
        );
    }

    #[test]
    fn dates() {
        let day = |secs: u64| date(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(951_782_400), "2000-02-29");
        assert_eq!(day(1_798_675_200), "2026-12-31");
        assert_eq!(date(UNIX_EPOCH - Duration::from_secs(10)), "1970-01-01");
    }
}
