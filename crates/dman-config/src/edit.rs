//! Comment-preserving config editing.

use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

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

    /// Remove the key at `path`; `true` if it existed. Other keys, comments
    /// and formatting are untouched.
    pub fn remove(&mut self, path: &[&str]) -> Result<bool, ConfigError> {
        let bad = || ConfigError::BadPath(path.join("."));
        let (last, parents) = path.split_last().ok_or_else(bad)?;
        let mut table: &mut Table = self.doc.as_table_mut();
        for key in parents {
            table = match table.get_mut(key) {
                Some(Item::Table(t)) => t,
                Some(_) => return Err(bad()),
                None => return Ok(false),
            };
        }
        Ok(table.remove(last).is_some())
    }

    /// Create or replace the `[[search.provider]]` entry named `table["name"]`
    /// (case-insensitive) with exactly `table`. Other entries, comments and
    /// formatting are untouched.
    pub fn upsert_search_provider(&mut self, table: &toml::Table) -> Result<(), ConfigError> {
        let name = provider_name(table)?;
        let new = to_edit_table(table)?;
        let list = self.provider_list()?;
        match find_provider(list, &name) {
            Some(i) => *list.get_mut(i).ok_or_else(bad_provider)? = new,
            None => list.push(new),
        }
        Ok(())
    }

    /// Set `fields` on the `[[search.provider]]` entry named `name`, creating
    /// `{ name, ..fields }` when none exists (a built-in override). Existing
    /// values keep their trailing comments.
    pub fn patch_search_provider(
        &mut self,
        name: &str,
        fields: &toml::Table,
    ) -> Result<(), ConfigError> {
        let list = self.provider_list()?;
        let Some(i) = find_provider(list, name) else {
            let mut all = toml::Table::new();
            all.insert("name".into(), toml::Value::String(name.to_owned()));
            all.extend(fields.iter().map(|(k, v)| (k.clone(), v.clone())));
            list.push(to_edit_table(&all)?);
            return Ok(());
        };
        let entry = list.get_mut(i).ok_or_else(bad_provider)?;
        let patch = to_edit_table(fields)?;
        for (k, item) in patch.iter() {
            if let (Some(Item::Value(old)), Item::Value(new)) = (entry.get(k), item) {
                let mut new = new.clone();
                *new.decor_mut() = old.decor().clone();
                entry[k] = Item::Value(new);
            } else {
                entry[k] = item.clone();
            }
        }
        Ok(())
    }

    /// Remove the `[[search.provider]]` entry named `name`; `true` if it existed.
    pub fn remove_search_provider(&mut self, name: &str) -> Result<bool, ConfigError> {
        let list = self.provider_list()?;
        match find_provider(list, name) {
            Some(i) => {
                list.remove(i);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn provider_list(&mut self) -> Result<&mut ArrayOfTables, ConfigError> {
        let search = self
            .doc
            .as_table_mut()
            .entry("search")
            .or_insert_with(|| {
                let mut t = Table::new();
                t.set_implicit(true);
                Item::Table(t)
            })
            .as_table_mut()
            .ok_or_else(bad_provider)?;
        search
            .entry("provider")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or_else(bad_provider)
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

fn bad_provider() -> ConfigError {
    ConfigError::BadPath("search.provider".into())
}

fn provider_name(t: &toml::Table) -> Result<String, ConfigError> {
    t.get("name")
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ConfigError::Invalid("search.provider entry without a `name`".into()))
}

fn find_provider(list: &ArrayOfTables, name: &str) -> Option<usize> {
    list.iter().position(|t| {
        t.get("name")
            .and_then(Item::as_str)
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    })
}

/// Convert a `toml` table into an editable one.
fn to_edit_table(t: &toml::Table) -> Result<Table, ConfigError> {
    let text = toml::to_string(t).map_err(|e| ConfigError::Invalid(e.to_string()))?;
    let doc = text
        .parse::<DocumentMut>()
        .map_err(|e| ConfigError::Invalid(e.to_string()))?;
    // Nested tables become inline tables (valid anywhere inside an array of
    // tables) and `name` goes first.
    let mut src = doc.as_table().clone();
    let mut out = Table::new();
    if let Some(n) = src.remove("name") {
        out["name"] = n;
    }
    for (k, item) in src {
        out[k.as_str()] = match item {
            Item::Table(t) => Item::Value(Value::InlineTable(t.into_inline_table())),
            other => other,
        };
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn remove_drops_only_that_key() {
        let mut doc = ConfigDoc::parse(
            "# top\n[video]\n# keep\ndefault_container = \"mkv\" # c\ndefault_quality = \"best\"\n",
            "c.toml",
        )
        .unwrap();
        assert!(doc.remove(&["video", "default_container"]).unwrap());
        assert!(!doc.remove(&["video", "default_container"]).unwrap());
        assert!(!doc.remove(&["nope", "x"]).unwrap());
        let t = doc.to_string_lossy();
        assert!(t.contains("# top") && t.contains("default_quality"));
        assert!(!t.contains("default_container"));
    }

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
        doc.set(&["future", "port"], 8080).unwrap();
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
        assert!(cfg.extra.contains_key("future"));

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

    #[test]
    fn provider_entries_upsert_patch_remove_keep_comments() {
        let text = "# top\n[ui]\nmouse = false\n\n# my providers\n[[search.provider]]\nname = \"Nyaa\"\nenabled = false # off for now\norder = 3\n";
        let mut doc = ConfigDoc::parse(text, "/x/config.toml").unwrap();
        let mut f = toml::Table::new();
        f.insert("enabled".into(), true.into());
        doc.patch_search_provider("nyaa", &f).unwrap();
        let mut f = toml::Table::new();
        f.insert("timeout_secs".into(), 9.into());
        doc.patch_search_provider("YTS", &f).unwrap();
        let custom: toml::Table = toml::from_str(
            "name = \"mine\"\nkind = \"feed\"\nurl = \"http://x/rss\"\n[headers]\nX-A = \"b\"\n",
        )
        .unwrap();
        doc.upsert_search_provider(&custom).unwrap();
        let out = doc.to_string_lossy();
        assert!(
            out.contains("# top") && out.contains("# my providers"),
            "{out}"
        );
        assert!(out.contains("enabled = true # off for now"), "{out}");
        let cfg = crate::Config::from_toml_str(&out, Path::new("c.toml")).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.search.provider.len(), 3);
        let defs = cfg.search.effective_providers().unwrap();
        assert!(defs.iter().any(|d| d.name == "Nyaa" && d.enabled));
        assert!(defs.iter().any(|d| d.name == "YTS" && d.timeout_secs == 9));
        assert!(
            defs.iter()
                .any(|d| d.name == "mine" && d.headers["X-A"] == "b")
        );
        // Replace, then remove.
        let custom2: toml::Table =
            toml::from_str("name = \"MINE\"\nkind = \"feed\"\nurl = \"http://y/rss\"\n").unwrap();
        doc.upsert_search_provider(&custom2).unwrap();
        assert!(doc.remove_search_provider("mine").unwrap());
        assert!(!doc.remove_search_provider("mine").unwrap());
        let cfg =
            crate::Config::from_toml_str(&doc.to_string_lossy(), Path::new("c.toml")).unwrap();
        assert_eq!(cfg.search.provider.len(), 2);
    }
}
