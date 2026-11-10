//! Custom theme files: `<config dir>/themes/<name>.toml`.
//!
//! ```toml
//! inherits = "dman-dark"   # a built-in theme (default: dman-dark)
//! accent = "#ff8800"       # token overrides, as in [ui.colors]
//! ```
//!
//! Problems never stop the TUI: the caller gets warnings and falls back to a
//! built-in theme.

use std::path::{Path, PathBuf};

/// A parsed theme file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeFile {
    /// The built-in theme it starts from.
    pub inherits: &'static str,
    /// Token overrides, in file order.
    pub colors: Vec<(String, String)>,
    /// Things that were ignored (unknown theme to inherit from, odd values).
    pub warnings: Vec<String>,
}

/// Whether `name` can name a theme file (no paths, no hidden files).
pub fn is_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
}

/// The file of theme `name` in `dir`.
pub fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.toml"))
}

/// Parse the text of a theme file.
///
/// # Errors
/// The TOML is invalid, or a value is not a table of strings.
pub fn parse(text: &str) -> Result<ThemeFile, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
        // The first line of the message is enough (the rest is a code frame).
        e.message().to_owned()
    })?;
    let mut warnings = Vec::new();
    let mut inherits = "dman-dark";
    let mut colors = Vec::new();
    let mut add = |k: &str, v: &toml::Value, warnings: &mut Vec<String>| match v.as_str() {
        Some(s) => colors.push((k.to_owned(), s.to_owned())),
        None => warnings.push(format!("'{k}' must be a colour string")),
    };
    for (k, v) in &table {
        match (k.as_str(), v) {
            ("inherits", toml::Value::String(s)) => match super::Theme::canonical(s) {
                Some(name) => inherits = name,
                None => warnings.push(format!(
                    "unknown theme '{s}' to inherit from, using dman-dark"
                )),
            },
            ("inherits", _) => warnings.push("'inherits' must be a theme name".to_owned()),
            ("colors", toml::Value::Table(t)) => {
                for (k, v) in t {
                    add(k, v, &mut warnings);
                }
            }
            (k, toml::Value::Table(_) | toml::Value::Array(_)) => {
                warnings.push(format!("ignored '{k}' (not a colour)"));
            }
            (k, v) => add(k, v, &mut warnings),
        }
    }
    Ok(ThemeFile {
        inherits,
        colors,
        warnings,
    })
}

/// Read theme `name` from `dir`.
///
/// # Errors
/// The file is missing, unreadable or invalid (the message names the file).
pub fn load(dir: &Path, name: &str) -> Result<ThemeFile, String> {
    let file = path(dir, name);
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inherits_and_tokens() {
        let f =
            parse("inherits = \"tokyo-night\"\naccent = \"#ff8800\"\n[colors]\ninfo = \"cyan\"\n")
                .unwrap();
        assert_eq!(f.inherits, "tokyo-night");
        assert_eq!(
            f.colors,
            vec![
                ("accent".to_owned(), "#ff8800".to_owned()),
                ("info".to_owned(), "cyan".to_owned())
            ]
        );
        assert!(f.warnings.is_empty());
    }

    #[test]
    fn defaults_and_oddities_warn_but_parse() {
        let f = parse("inherits = \"nope\"\naccent = 5\n[x]\na = 1\n").unwrap();
        assert_eq!(f.inherits, "dman-dark");
        assert_eq!(f.warnings.len(), 3, "{:?}", f.warnings);
    }

    #[test]
    fn invalid_toml_is_an_error() {
        assert!(parse("accent = ").is_err());
    }

    #[test]
    fn file_names_cannot_escape_the_directory() {
        assert!(is_file_name("my-theme"));
        assert!(!is_file_name("../x"));
        assert!(!is_file_name("a/b"));
        assert!(!is_file_name(".hidden"));
        assert!(!is_file_name(""));
    }
}
