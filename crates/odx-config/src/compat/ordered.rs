use std::fmt;
use std::marker::PhantomData;

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};

/// A YAML mapping that keeps the order of its keys.
///
/// git-aggregator applies repositories in file order, and import output
/// should follow the file the user wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ordered<V>(pub(crate) Vec<(String, V)>);

impl<'de, V: Deserialize<'de>> Deserialize<'de> for Ordered<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OrderedVisitor<V>(PhantomData<V>);

        impl<'de, V: Deserialize<'de>> Visitor<'de> for OrderedVisitor<V> {
            type Value = Ordered<V>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a mapping")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(Ordered(entries))
            }
        }

        deserializer.deserialize_map(OrderedVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::Ordered;

    #[test]
    fn keeps_key_order() {
        let map: Ordered<u8> = serde_saphyr::from_str("b: 1\na: 2\nc: 3\n").unwrap();
        let keys: Vec<_> = map.0.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["b", "a", "c"]);
    }

    #[test]
    fn rejects_non_mappings() {
        #[derive(Debug, serde::Deserialize)]
        struct Wrapper {
            #[expect(dead_code, reason = "only the error matters")]
            x: Ordered<u8>,
        }
        let err = toml::from_str::<Wrapper>("x = 1").unwrap_err();
        assert!(err.to_string().contains("a mapping"), "{err}");
        let err = serde_saphyr::from_str::<Ordered<u8>>("a: x\n").unwrap_err();
        assert_ne!(err.to_string(), "");
    }
}
