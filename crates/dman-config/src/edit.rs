//! Comment-preserving config editing (decision Q5).

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, Value};

use crate::{ConfigError, Paths, fsutil};

/// A config file opened for editing with `toml_edit`, preserving comments,
/// ordering and formatting of everything not touched.
#[derive(Debug, Clone)]
pub struct ConfigDoc {
    path: PathBuf,
    doc: DocumentMut,
}

impl ConfigDoc {
    /// Open `config.toml`; a missing file yields an empty document.
    pub fn open(paths: &Paths) -> Result<Self, ConfigError> {
        let path = paths.config_file();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(ConfigError::io(path, e)),
        };
        Self::parse(&text, path)
    }

    /// Parse `text` as a document that will be saved to `path`.
    pub fn parse(text: &str, path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();
        let doc = text
            .parse::<DocumentMut>()
            .map_err(|e| ConfigError::Parse {
                path: path.clone(),
                message: e.to_string().trim_end().to_owned(),
                span: e.span(),
            })?;
        Ok(Self { path, doc })
    }

    /// Set the value at `path` (e.g. `["ui", "mouse"]`), creating intermediate
    /// tables as needed. Comments attached to an existing value are kept.
    pub fn set(&mut self, path: &[&str], value: impl Into<Value>) -> Result<(), ConfigError> {
        let bad = || ConfigError::BadPath(path.join("."));
        let (last, parents) = path.split_last().ok_or_else(bad)?;
        let mut table: &mut Table = self.doc.as_table_mut();
        for key in parents {
            let item = table
                .entry(key)
                .or_insert_with(|| Item::Table(Table::new()));
            table = match item {
                Item::Table(t) => t,
                _ => return Err(bad()),
            };
        }
        let mut new: Value = value.into();
        if let Some(Item::Value(old)) = table.get(last) {
            *new.decor_mut() = old.decor().clone();
        }
        // Assign through the existing slot so the key's own decor (leading
        // comments) is kept.
        table[*last] = Item::Value(new);
        Ok(())
    }

    /// The current document text.
    pub fn to_string_lossy(&self) -> String {
        self.doc.to_string()
    }

    /// Target file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Atomically write the document (temp file + rename).
    pub fn save(&self) -> Result<(), ConfigError> {
        fsutil::atomic_write(&self.path, self.doc.to_string().as_bytes(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn comments_survive_edit_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        Config::write_default(&paths).unwrap();

        let mut doc = ConfigDoc::open(&paths).unwrap();
        doc.set(&["ui", "mouse"], true).unwrap();
        doc.set(&["profiles", "local", "download_dir"], "~/Stuff")
            .unwrap();
        doc.set(&["limits", "download"], "10MiB").unwrap();
        doc.set(&["video", "engine"], "yt-dlp").unwrap();
        doc.save().unwrap();

        let text = std::fs::read_to_string(paths.config_file()).unwrap();
        assert!(text.contains("# dman configuration."));
        assert!(text.contains("# download_dir supports a leading \"~\"."));
        assert!(text.contains("# A remote daemon:"));
        assert!(text.contains("mouse = true"));

        let cfg = Config::load(&paths).unwrap();
        cfg.validate().unwrap();
        assert!(cfg.ui.mouse);
        assert_eq!(cfg.profiles["local"].download_dir, "~/Stuff");
        assert_eq!(cfg.limits.download.0, 10 << 20);
        assert!(cfg.extra.contains_key("video"));

        // No stray temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn inline_comment_on_replaced_value_is_kept() {
        let mut doc =
            ConfigDoc::parse("[ui]\nmouse = false # toggle me\n", "/x/config.toml").unwrap();
        doc.set(&["ui", "mouse"], true).unwrap();
        assert!(doc.to_string_lossy().contains("mouse = true # toggle me"));
    }

    #[test]
    fn bad_paths_rejected() {
        let mut doc = ConfigDoc::parse("a = 1\n", "/x/config.toml").unwrap();
        assert!(doc.set(&[], 1).is_err());
        assert!(doc.set(&["a", "b"], 1).is_err());
    }
}
