//! Reading `__manifest__.py` without running Python.
//!
//! Manifests use a small literal subset: a dict of strings, numbers,
//! booleans, `None`, lists, tuples and nested dicts, with comments and
//! implicit string concatenation.

use std::collections::BTreeMap;

/// A Python literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A string, after escapes and concatenation.
    Str(String),
    /// A number, as written.
    Number(String),
    /// `True` or `False`.
    Bool(bool),
    /// `None`.
    None,
    /// A list or tuple.
    List(Vec<Self>),
    /// A dict, in order.
    Dict(Vec<(Self, Self)>),
}

impl Value {
    /// The value under string key `key` of a dict.
    pub fn get(&self, key: &str) -> Option<&Self> {
        let Self::Dict(items) = self else {
            return None;
        };
        items
            .iter()
            .find(|(k, _)| *k == Self::Str(key.to_owned()))
            .map(|(_, value)| value)
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(text) => Some(text),
            _ => None,
        }
    }

    /// The strings of a list (other items are skipped); empty otherwise.
    pub fn strings(&self) -> Vec<String> {
        match self {
            Self::List(items) => items
                .iter()
                .filter_map(Self::as_str)
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// What odx needs from a manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    /// `version`.
    pub version: Option<String>,
    /// `depends`.
    pub depends: Vec<String>,
    /// `external_dependencies["python"]`, as written.
    pub python: Vec<String>,
    /// `installable` (default `true`).
    pub installable: bool,
}

/// Parse a manifest.
///
/// # Errors
///
/// A message with the byte offset when the text is not a dict literal.
pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let value = parse_literal(text)?;
    if !matches!(value, Value::Dict(_)) {
        return Err("the manifest is not a dict".to_owned());
    }
    let field = |key: &str| value.get(key).map(Value::strings).unwrap_or_default();
    Ok(Manifest {
        version: value
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned),
        depends: field("depends"),
        python: value
            .get("external_dependencies")
            .and_then(|deps| deps.get("python"))
            .map(Value::strings)
            .unwrap_or_default(),
        installable: value.get("installable") != Some(&Value::Bool(false)),
    })
}

/// Parse one Python literal that makes up all of `text`.
///
/// # Errors
///
/// A message with the byte offset of the first unexpected character.
pub fn parse_literal(text: &str) -> Result<Value, String> {
    let mut parser = Parser {
        chars: text.char_indices().collect(),
        at: 0,
    };
    let value = parser.value()?;
    parser.skip();
    match parser.peek() {
        None => Ok(value),
        Some(_) => Err(parser.error("end of the literal")),
    }
}

struct Parser {
    chars: Vec<(usize, char)>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).map(|(_, c)| *c)
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).map(|(_, c)| *c)
    }

    /// The characters from `start` up to here.
    fn text(&self, start: usize) -> String {
        self.chars
            .get(start..self.at)
            .unwrap_or_default()
            .iter()
            .map(|(_, c)| c)
            .collect()
    }

    fn error(&self, expected: &str) -> String {
        let offset = self.chars.get(self.at).map_or_else(
            || self.chars.last().map_or(0, |(i, c)| i + c.len_utf8()),
            |(i, _)| *i,
        );
        format!("expected {expected} at byte {offset}")
    }

    /// Skip whitespace and comments.
    fn skip(&mut self) {
        while let Some(c) = self.peek() {
            if c == '#' {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.at += 1;
                }
            } else if c.is_whitespace() || c == '\\' {
                self.at += 1;
            } else {
                break;
            }
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.skip();
        match self.peek() {
            Some('{') => self.dict(),
            Some('[') => self.sequence(']'),
            Some('(') => self.sequence(')'),
            Some(c) if c == '"' || c == '\'' || self.string_prefix() => self.strings(),
            Some(c) if c.is_ascii_digit() || matches!(c, '-' | '+' | '.') => Ok(self.number()),
            Some(c) if c.is_alphabetic() => self.name(),
            _ => Err(self.error("a value")),
        }
    }

    /// Whether a string prefix (`r`, `u`, `b`, `rb`, ...) starts here.
    fn string_prefix(&self) -> bool {
        let prefix = (0..2)
            .map_while(|i| self.peek_at(i).filter(|c| "rRuUbB".contains(*c)))
            .count();
        prefix > 0 && matches!(self.peek_at(prefix), Some('"' | '\''))
    }

    fn dict(&mut self) -> Result<Value, String> {
        self.at += 1;
        let mut items = Vec::new();
        loop {
            self.skip();
            if self.peek() == Some('}') {
                self.at += 1;
                return Ok(Value::Dict(items));
            }
            let key = self.value()?;
            self.skip();
            if self.peek() != Some(':') {
                return Err(self.error("':'"));
            }
            self.at += 1;
            let value = self.value()?;
            items.push((key, value));
            if !self.comma('}') {
                return Err(self.error("',' or '}'"));
            }
        }
    }

    fn sequence(&mut self, close: char) -> Result<Value, String> {
        self.at += 1;
        let mut items = Vec::new();
        let mut trailing_comma = false;
        loop {
            self.skip();
            if self.peek() == Some(close) {
                self.at += 1;
                // `(x)` is x itself; `(x,)` and `[x]` are sequences.
                return Ok(match items.pop() {
                    Some(only) if close == ')' && items.is_empty() && !trailing_comma => only,
                    Some(last) => {
                        items.push(last);
                        Value::List(items)
                    }
                    None => Value::List(items),
                });
            }
            items.push(self.value()?);
            self.skip();
            trailing_comma = self.peek() == Some(',');
            if !self.comma(close) {
                return Err(self.error(&format!("',' or '{close}'")));
            }
        }
    }

    /// After an item: consume a comma, or accept the closing bracket.
    fn comma(&mut self, close: char) -> bool {
        self.skip();
        match self.peek() {
            Some(',') => {
                self.at += 1;
                true
            }
            Some(c) => c == close,
            None => false,
        }
    }

    /// Adjacent string literals, concatenated.
    fn strings(&mut self) -> Result<Value, String> {
        let mut text = self.string()?;
        loop {
            self.skip();
            match self.peek() {
                Some('"' | '\'') => text.push_str(&self.string()?),
                Some(_) if self.string_prefix() => text.push_str(&self.string()?),
                _ => return Ok(Value::Str(text)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        let mut raw = false;
        while let Some(c) = self.peek().filter(|c| "rRuUbB".contains(*c)) {
            raw |= matches!(c, 'r' | 'R');
            self.at += 1;
        }
        let quote = self.peek().unwrap_or('"');
        let triple = self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote);
        self.at += if triple { 3 } else { 1 };
        let mut text = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.error("the end of the string"));
            };
            if c == quote
                && (!triple || (self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote)))
            {
                self.at += if triple { 3 } else { 1 };
                return Ok(text);
            }
            if c == '\n' && !triple {
                return Err(self.error("the end of the string"));
            }
            self.at += 1;
            if c == '\\' && !raw {
                let escaped = self.peek().unwrap_or('\\');
                self.at += 1;
                match escaped {
                    'n' => text.push('\n'),
                    't' => text.push('\t'),
                    '\n' => {}
                    '\\' | '\'' | '"' => text.push(escaped),
                    other => {
                        text.push('\\');
                        text.push(other);
                    }
                }
            } else {
                text.push(c);
            }
        }
    }

    fn number(&mut self) -> Value {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
        {
            self.at += 1;
        }
        Value::Number(self.text(start))
    }

    fn name(&mut self) -> Result<Value, String> {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            self.at += 1;
        }
        let name = self.text(start);
        match name.as_str() {
            "True" => Ok(Value::Bool(true)),
            "False" => Ok(Value::Bool(false)),
            "None" => Ok(Value::None),
            _ => {
                self.at = start;
                Err(self.error("a literal"))
            }
        }
    }
}

/// `text` (a `__manifest__.py`) with the string after the `version` key
/// replaced by `version`; `None` when there is no such string.
pub fn set_manifest_version(text: &str, version: &str) -> Option<String> {
    ["\"version\"", "'version'"].iter().find_map(|key| {
        let (before, after) = text.split_once(key)?;
        let (gap, value) = after.split_once(':')?;
        let (lead, quoted) = value.split_at(value.find(['"', '\''])?);
        let quote = if quoted.starts_with('"') { '"' } else { '\'' };
        let (_, inner) = quoted.split_at(1);
        let (_, rest) = inner.split_once(quote)?;
        Some(format!(
            "{before}{key}{gap}:{lead}{quote}{version}{quote}{rest}"
        ))
    })
}

/// Import names in `external_dependencies["python"]` that differ from the
/// distribution that provides them; manifests written for pip already use
/// distribution names, which pass through unchanged.
const IMPORT_TO_DISTRIBUTION: &[(&str, &str)] = &[
    ("Bio", "biopython"),
    ("Crypto", "pycryptodome"),
    ("Levenshtein", "python-Levenshtein"),
    ("MySQLdb", "mysqlclient"),
    ("OpenSSL", "pyOpenSSL"),
    ("PIL", "Pillow"),
    ("barcode", "python-barcode"),
    ("bs4", "beautifulsoup4"),
    ("cups", "pycups"),
    ("cv2", "opencv-python"),
    ("dateutil", "python-dateutil"),
    ("dns", "dnspython"),
    ("docx", "python-docx"),
    ("git", "GitPython"),
    ("jose", "python-jose"),
    ("jwt", "PyJWT"),
    ("ldap", "python-ldap"),
    ("magic", "python-magic"),
    ("odf", "odfpy"),
    ("pptx", "python-pptx"),
    ("serial", "pyserial"),
    ("sklearn", "scikit-learn"),
    ("slugify", "python-slugify"),
    ("stdnum", "python-stdnum"),
    ("usb", "pyusb"),
    ("yaml", "PyYAML"),
    ("zmq", "pyzmq"),
];

/// The distribution behind an `external_dependencies["python"]` entry and
/// its version specifier: `("python-dateutil", "")` for `dateutil`,
/// `("requests", ">=2.31")` for `requests>=2.31`.
pub fn python_requirement(entry: &str) -> (String, String) {
    let entry = entry.trim();
    let end = entry
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(entry.len());
    let (name, rest) = entry.split_at(end);
    let mapped: BTreeMap<&str, &str> = IMPORT_TO_DISTRIBUTION.iter().copied().collect();
    let name = mapped.get(name).copied().unwrap_or(name);
    let specifier = rest
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches('[')
        .rsplit(']')
        .next()
        .unwrap_or_default()
        .trim();
    (name.to_owned(), specifier.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OCA: &str = r#"# Copyright 2024 Someone
# License AGPL-3.0 or later (https://www.gnu.org/licenses/agpl).
{
    "name": "POS X",
    "summary": "Does " 'things'
    """ and more""",
    "version": "17.0.1.2.0",
    "category": 'Point of Sale',
    "license": "AGPL-3",
    "depends": ["point_of_sale", 'base_x',],
    "external_dependencies": {"python": ["dateutil", "requests>=2.31"], "bin": ()},
    "data": [
        # views
        "views/x.xml",
    ],
    "installable": True,
    "application": False,
    "sequence": -1,
    "price": 1.5e3,
    "images": None,
    "maintainers": ("a", "b"),
    "grouped": ("x"),
    "raw": r"\d+\n",
    "escaped": "a\"b\\c\nd\te\q",
    "continued": "a\
b",
    'pre': b'x' u"y",
}
"#;

    #[test]
    fn oca_manifest() {
        let manifest = parse_manifest(OCA).unwrap();
        assert_eq!(manifest.version.as_deref(), Some("17.0.1.2.0"));
        assert_eq!(manifest.depends, ["point_of_sale", "base_x"]);
        assert_eq!(manifest.python, ["dateutil", "requests>=2.31"]);
        assert!(manifest.installable);
        let value = parse_literal(OCA).unwrap();
        assert_eq!(
            value.get("summary").and_then(Value::as_str),
            Some("Does things and more")
        );
        assert_eq!(value.get("sequence"), Some(&Value::Number("-1".to_owned())));
        assert_eq!(value.get("price"), Some(&Value::Number("1.5e3".to_owned())));
        assert_eq!(value.get("images"), Some(&Value::None));
        assert_eq!(value.get("grouped").and_then(Value::as_str), Some("x"));
        assert_eq!(
            value.get("maintainers").map(Value::strings).unwrap(),
            ["a", "b"]
        );
        assert_eq!(value.get("raw").and_then(Value::as_str), Some("\\d+\\n"));
        assert_eq!(
            value.get("escaped").and_then(Value::as_str),
            Some("a\"b\\c\nd\te\\q")
        );
        assert_eq!(value.get("continued").and_then(Value::as_str), Some("ab"));
        assert_eq!(value.get("pre").and_then(Value::as_str), Some("xy"));
        assert_eq!(value.get("missing"), None);
        assert_eq!(
            value.get("data").map(Value::strings).unwrap(),
            ["views/x.xml"]
        );
        assert_eq!(
            value.get("installable").unwrap().strings(),
            Vec::<String>::new()
        );
        assert_eq!(Value::None.get("x"), None);
        assert_eq!(Value::None.as_str(), None);
        assert_eq!(parse_literal("[]").unwrap(), Value::List(Vec::new()));
        assert_eq!(parse_literal("()").unwrap(), Value::List(Vec::new()));
        assert_eq!(
            parse_literal("(1,)").unwrap(),
            Value::List(vec![Value::Number("1".to_owned())])
        );
    }

    #[test]
    fn minimal_and_uninstallable_manifests() {
        let manifest = parse_manifest("{'name': 'x', 'installable': False}").unwrap();
        assert_eq!(manifest.version, None);
        assert!(manifest.depends.is_empty() && manifest.python.is_empty());
        assert!(!manifest.installable);
        assert_eq!(
            parse_manifest("{}").unwrap(),
            Manifest {
                installable: true,
                ..Manifest::default()
            }
        );
    }

    #[test]
    fn invalid_manifests() {
        for (text, error) in [
            ("[1]", "the manifest is not a dict"),
            ("{'a' 1}", "expected ':' at byte 5"),
            ("{'a': 1 'b'}", "expected ',' or '}' at byte 8"),
            ("[1 2]", "expected ',' or ']' at byte 3"),
            ("{'a': }", "expected a value at byte 6"),
            ("{'a': open('x')}", "expected a literal at byte 6"),
            ("{'a': 'x", "expected the end of the string at byte 8"),
            ("{'a': 'x\n'}", "expected the end of the string at byte 8"),
            ("{} {}", "expected end of the literal at byte 3"),
            ("{'a': 1", "expected ',' or '}' at byte 7"),
            ("", "expected a value at byte 0"),
            ("{'a': bad}", "expected a literal at byte 6"),
            ("{: 1}", "expected a value at byte 1"),
            ("[,]", "expected a value at byte 1"),
            ("{'a': 'x' 'y}", "expected the end of the string at byte 13"),
            (
                "{'a': 'x' r'y}",
                "expected the end of the string at byte 14",
            ),
        ] {
            assert_eq!(parse_manifest(text).unwrap_err(), error, "{text:?}");
        }
    }

    #[test]
    fn manifest_versions_are_replaced_in_place() {
        let text = "{\n    \"name\": \"X\",\n    \"version\": \"17.0.1.2.0\",  # bumped\n}\n";
        assert_eq!(
            set_manifest_version(text, "17.0.1.2.1").unwrap(),
            "{\n    \"name\": \"X\",\n    \"version\": \"17.0.1.2.1\",  # bumped\n}\n"
        );
        assert_eq!(
            set_manifest_version("{'version' : '1.0'}", "1.1").unwrap(),
            "{'version' : '1.1'}"
        );
        assert_eq!(set_manifest_version("{'name': 'x'}", "1"), None);
        assert_eq!(set_manifest_version("{'version'", "1"), None);
        assert_eq!(set_manifest_version("{'version': 1}", "1"), None);
        assert_eq!(set_manifest_version("{'version': '1", "1"), None);
    }

    #[test]
    fn python_requirements() {
        assert_eq!(
            python_requirement("dateutil"),
            ("python-dateutil".to_owned(), String::new())
        );
        assert_eq!(
            python_requirement(" requests>=2.31,<3 ; python_version > '3.8'"),
            ("requests".to_owned(), ">=2.31,<3".to_owned())
        );
        assert_eq!(
            python_requirement("qrcode[pil]>=7"),
            ("qrcode".to_owned(), ">=7".to_owned())
        );
    }
}
