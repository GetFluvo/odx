use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, de};

/// A duration written as a number and a unit: `"90s"`, `"15m"`, `"24h"`,
/// `"7d"` or `"2w"`.
///
/// ```
/// use odx_config::Span;
/// use std::time::Duration;
///
/// let span: Span = "15m".parse().unwrap();
/// assert_eq!(span.as_duration(), Duration::from_secs(15 * 60));
/// assert_eq!(span.to_string(), "15m");
/// assert!("15 minutes".parse::<Span>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    amount: u64,
    unit: Unit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Unit {
    Seconds,
    Minutes,
    Hours,
    Days,
    Weeks,
}

impl Unit {
    const fn seconds(self) -> u64 {
        match self {
            Self::Seconds => 1,
            Self::Minutes => 60,
            Self::Hours => 60 * 60,
            Self::Days => 24 * 60 * 60,
            Self::Weeks => 7 * 24 * 60 * 60,
        }
    }

    const fn suffix(self) -> char {
        match self {
            Self::Seconds => 's',
            Self::Minutes => 'm',
            Self::Hours => 'h',
            Self::Days => 'd',
            Self::Weeks => 'w',
        }
    }
}

impl Span {
    /// The span as a standard [`Duration`], saturating on overflow.
    pub const fn as_duration(self) -> Duration {
        Duration::from_secs(self.amount.saturating_mul(self.unit.seconds()))
    }
}

/// The error returned when a duration string is malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid duration {0:?}: expected a whole number followed by s, m, h, d or w, e.g. \"15m\""
)]
pub struct SpanError(String);

impl FromStr for Span {
    type Err = SpanError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let error = || SpanError(text.to_owned());
        let mut chars = text.chars();
        let unit = match chars.next_back().ok_or_else(error)? {
            's' => Unit::Seconds,
            'm' => Unit::Minutes,
            'h' => Unit::Hours,
            'd' => Unit::Days,
            'w' => Unit::Weeks,
            _ => return Err(error()),
        };
        let digits = chars.as_str();
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(error());
        }
        let amount = digits.parse().map_err(|_| error())?;
        Ok(Self { amount, unit })
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.amount, self.unit.suffix())
    }
}

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unit_parses_and_displays() {
        for (text, secs) in [
            ("90s", 90),
            ("15m", 900),
            ("24h", 86_400),
            ("7d", 604_800),
            ("2w", 1_209_600),
        ] {
            let span: Span = text.parse().unwrap();
            assert_eq!(span.as_duration(), Duration::from_secs(secs));
            assert_eq!(span.to_string(), text);
        }
    }

    #[test]
    fn malformed_spans_are_rejected() {
        for text in [
            "",
            "m",
            "15",
            "15x",
            "-5m",
            "1.5h",
            "99999999999999999999999d",
        ] {
            let err = text.parse::<Span>().unwrap_err();
            assert!(err.to_string().contains("invalid duration"), "{text}");
        }
    }

    #[test]
    fn huge_spans_saturate() {
        let span: Span = "18446744073709551615w".parse().unwrap();
        assert_eq!(span.as_duration(), Duration::from_secs(u64::MAX));
    }

    #[test]
    fn deserializes_from_toml_string() {
        #[derive(Debug, Deserialize)]
        struct Wrapper {
            ttl: Span,
        }
        let ok: Wrapper = toml::from_str("ttl = \"6h\"").unwrap();
        assert_eq!(ok.ttl.to_string(), "6h");
        let err = toml::from_str::<Wrapper>("ttl = \"6 hours\"").unwrap_err();
        assert!(err.to_string().contains("invalid duration"));
        let err = toml::from_str::<Wrapper>("ttl = 6").unwrap_err();
        assert!(err.to_string().contains("invalid type"));
    }
}
