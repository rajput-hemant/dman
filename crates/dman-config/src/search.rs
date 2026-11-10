//! `[search]`: providers (`[[search.provider]]`), tracker list and history.
//!
//! The effective provider list is the built-in set (`dman_search::builtin_providers`)
//! with the `[[search.provider]]` entries merged in by name (case-insensitive):
//! an entry naming a built-in only needs the fields it overrides (`enabled`,
//! `order`, `timeout_secs`, ...); any other name is a custom provider and must
//! be a complete definition. API keys are never written here: Torznab
//! providers reference an entry of `secrets.toml` with `api_key_ref`.

use std::collections::HashSet;

use dman_search::{ProviderDef, ProxyUrl, builtin_providers};
use serde::{Deserialize, Serialize};

fn default_tracker_url() -> String {
    dman_search::DEFAULT_TRACKER_LIST_URL.to_owned()
}

fn default_refresh() -> u64 {
    24
}

fn yes() -> bool {
    true
}

/// `[search.tracker_list]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrackerList {
    /// Fetch the list at start and every `refresh_hours`. Off by default:
    /// dman makes no network requests beyond providers and engines.
    pub enabled: bool,
    /// Plain text, one tracker URL per line.
    #[serde(default = "default_tracker_url")]
    pub url: String,
    /// Hours between refreshes.
    #[serde(default = "default_refresh")]
    pub refresh_hours: u64,
    /// Add the list as `bt-tracker` to new torrent and magnet jobs.
    #[serde(default = "yes")]
    pub attach_to_new: bool,
}

impl Default for TrackerList {
    fn default() -> Self {
        Self {
            enabled: false,
            url: default_tracker_url(),
            refresh_hours: default_refresh(),
            attach_to_new: true,
        }
    }
}

fn default_history() -> usize {
    50
}

/// The `[search]` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Search {
    /// `[[search.provider]]` entries, kept raw so partial built-in overrides
    /// round-trip. Use [`Search::effective_providers`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provider: Vec<toml::Table>,
    /// Tracker list source.
    pub tracker_list: TrackerList,
    /// Proxy for provider and tracker-list requests (`socks5h://host:port`,
    /// `http://...`). A provider's own `proxy` wins; `proxy = "none"` on a
    /// provider connects directly. Credentials stay out of `Debug` output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyUrl>,
    /// Name resolution for provider and tracker-list requests: `system`
    /// (default), `doh:cloudflare`, `doh:quad9` or `doh:https://host/dns-query`
    /// (DNS over HTTPS, for networks that tamper with DNS). Not applied to
    /// aria2, which has its own `async-dns-server` option.
    pub dns: String,
    /// Queries kept in the search history (0 = none).
    pub history_size: usize,
}

impl Default for Search {
    fn default() -> Self {
        Self {
            provider: Vec::new(),
            tracker_list: TrackerList::default(),
            proxy: None,
            dns: "system".to_owned(),
            history_size: default_history(),
        }
    }
}

/// Every key a provider definition understands (to catch typos; the
/// definition type itself ignores unknown keys).
const KNOWN_KEYS: &[&str] = &[
    "name",
    "kind",
    "enabled",
    "timeout_secs",
    "rate_limit_per_min",
    "max_results",
    "order",
    "test_query",
    "verified",
    "description",
    "headers",
    "mirrors",
    "proxy",
    "local_filter",
    "url",
    "api_key_ref",
    "categories",
    "search_url",
    "row",
    "title",
    "magnet",
    "detail_link",
    "size",
    "seeders",
    "leechers",
    "date",
    "date_format",
    "follow_detail_for_magnet",
    "detail_magnet",
    "detail_limit",
    "method",
    "body",
    "items",
    "expand",
    "info_hash",
    "torrent_url",
    "detail_url",
    "category",
];

fn entry_name(i: usize, t: &toml::Table) -> Result<&str, String> {
    match t.get("name") {
        Some(toml::Value::String(s)) if !s.trim().is_empty() => Ok(s),
        Some(_) => Err(format!(
            "search.provider #{}: `name` must be a non-empty string",
            i + 1
        )),
        None => Err(format!("search.provider #{}: missing `name`", i + 1)),
    }
}

fn def_to_table(def: &ProviderDef) -> Result<toml::Table, String> {
    toml::Table::try_from(def).map_err(|e| format!("provider `{}`: {e}", def.name))
}

fn table_to_def(name: &str, t: toml::Table, builtin: bool) -> Result<ProviderDef, String> {
    toml::Value::Table(t).try_into::<ProviderDef>().map_err(|e| {
        let msg = e.to_string();
        let msg = msg.trim();
        if !builtin && msg.contains("missing field") {
            format!(
                "search.provider `{name}` is not a built-in provider, so it needs a full definition ({msg}); \
                 built-in names are: {}",
                builtin_providers()
                    .iter()
                    .map(|d| d.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            format!("search.provider `{name}`: {msg}")
        }
    })
}

/// Apply one `[[search.provider]]` entry to the working list: an override of a
/// built-in (by name) or a custom definition.
fn apply_entry(
    defs: &mut Vec<(ProviderDef, bool)>,
    i: usize,
    entry: &toml::Table,
) -> Result<(), String> {
    let name = entry_name(i, entry)?.to_owned();
    if let Some(k) = entry.keys().find(|k| !KNOWN_KEYS.contains(&k.as_str())) {
        return Err(format!("search.provider `{name}`: unknown field `{k}`"));
    }
    let pos = defs
        .iter()
        .position(|(d, b)| *b && d.name.eq_ignore_ascii_case(&name));
    if let Some(pos) = pos {
        let mut merged = def_to_table(&defs[pos].0)?;
        for (k, v) in entry {
            if k == "name" {
                continue;
            }
            if k == "kind" && merged.get("kind") != Some(v) {
                return Err(format!(
                    "search.provider `{name}`: the kind of a built-in provider cannot be changed \
                     (use a different name for a custom provider)"
                ));
            }
            merged.insert(k.clone(), v.clone());
        }
        defs[pos].0 = table_to_def(&name, merged, true)?;
    } else {
        defs.push((table_to_def(&name, entry.clone(), false)?, false));
    }
    Ok(())
}

impl Search {
    /// Built-ins merged with the `[[search.provider]]` overrides, plus custom
    /// providers; every definition is validated. The flag says whether a
    /// provider is a built-in. Sorted by `order`, then name.
    ///
    /// # Errors
    /// A message naming the offending entry and field.
    pub fn effective_providers_flagged(&self) -> Result<Vec<(ProviderDef, bool)>, String> {
        let mut defs: Vec<(ProviderDef, bool)> =
            builtin_providers().into_iter().map(|d| (d, true)).collect();
        let mut seen = HashSet::new();
        for (i, entry) in self.provider.iter().enumerate() {
            let name = entry_name(i, entry)?.to_lowercase();
            if !seen.insert(name.clone()) {
                return Err(format!(
                    "search.provider `{}` is defined twice",
                    entry_name(i, entry)?
                ));
            }
            apply_entry(&mut defs, i, entry)?;
        }
        let mut names = HashSet::new();
        for (d, _) in &defs {
            d.validate().map_err(|e| format!("search.provider {e}"))?;
            if !names.insert(d.name.to_lowercase()) {
                return Err(format!("search.provider `{}` is defined twice", d.name));
            }
        }
        defs.sort_by(|a, b| {
            a.0.order
                .cmp(&b.0.order)
                .then_with(|| a.0.name.cmp(&b.0.name))
        });
        Ok(defs)
    }

    /// [`Search::effective_providers_flagged`] without the built-in flags.
    ///
    /// # Errors
    /// See [`Search::effective_providers_flagged`].
    pub fn effective_providers(&self) -> Result<Vec<ProviderDef>, String> {
        Ok(self
            .effective_providers_flagged()?
            .into_iter()
            .map(|(d, _)| d)
            .collect())
    }

    /// Semantic checks: providers, tracker list, proxy, history size.
    ///
    /// # Errors
    /// A human-readable message.
    pub fn validate(&self) -> Result<(), String> {
        self.effective_providers_flagged()?;
        let t = &self.tracker_list;
        match url::Url::parse(&t.url) {
            Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => {}
            _ => {
                return Err(format!(
                    "search.tracker_list.url `{}` must be an http(s) URL",
                    t.url
                ));
            }
        }
        if t.refresh_hours == 0 || t.refresh_hours > 24 * 90 {
            return Err("search.tracker_list.refresh_hours must be between 1 and 2160".into());
        }
        if let Some(p) = &self.proxy {
            p.validate().map_err(|e| format!("search.{e}"))?;
        }
        dman_search::DnsMode::parse(&self.dns).map_err(|e| format!("search.dns: {e}"))?;
        if self.history_size > 10_000 {
            return Err("search.history_size must be at most 10000".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{Config, ConfigError};

    fn parse(s: &str) -> Config {
        Config::from_toml_str(s, std::path::Path::new("config.toml")).unwrap()
    }

    fn invalid(s: &str) -> String {
        match parse(s).validate() {
            Err(ConfigError::Invalid(m)) => m,
            other => panic!("expected an invalid config, got {other:?}"),
        }
    }

    #[test]
    fn defaults_have_all_builtins_disabled() {
        let c = Config::default();
        assert!(!c.search.tracker_list.enabled);
        assert!(c.search.tracker_list.attach_to_new);
        assert_eq!(c.search.tracker_list.refresh_hours, 24);
        assert_eq!(c.search.history_size, 50);
        let defs = c.search.effective_providers().unwrap();
        assert!(defs.len() >= 5);
        assert!(defs.iter().all(|d| !d.enabled));
    }

    #[test]
    fn builtin_override_by_name_is_partial_and_case_insensitive() {
        let c = parse(
            "[[search.provider]]\nname = \"nyaa\"\nenabled = true\norder = -5\ntimeout_secs = 7\n",
        );
        c.validate().unwrap();
        let defs = c.search.effective_providers().unwrap();
        let nyaa = defs.iter().find(|d| d.name == "Nyaa").unwrap();
        assert!(nyaa.enabled);
        assert_eq!((nyaa.order, nyaa.timeout_secs), (-5, 7));
        // The rest of the built-in definition is intact; order puts it first.
        assert_eq!(defs[0].name, "Nyaa");
        assert!(defs.iter().filter(|d| d.enabled).count() == 1);
        let text = toml::to_string(&c).unwrap();
        assert_eq!(parse(&text), c);
    }

    #[test]
    fn custom_provider_is_added() {
        let c = parse(
            "[[search.provider]]\nname = \"mine\"\nkind = \"torznab\"\nurl = \"http://localhost:9696/api\"\napi_key_ref = \"prowlarr\"\nenabled = true\n",
        );
        c.validate().unwrap();
        let flagged = c.search.effective_providers_flagged().unwrap();
        let (d, builtin) = flagged.iter().find(|(d, _)| d.name == "mine").unwrap();
        assert!(!builtin && d.enabled);
        assert!(flagged.iter().any(|(_, b)| *b));
    }

    #[test]
    fn errors_are_specific() {
        let m = invalid("[[search.provider]]\nname = \"mine\"\nenabled = true\n");
        assert!(m.contains("`mine`") && m.contains("full definition"), "{m}");
        let m = invalid("[[search.provider]]\nenabled = true\n");
        assert!(m.contains("missing `name`"), "{m}");
        let m = invalid("[[search.provider]]\nname = \"Nyaa\"\ntimeout_secs = 0\n");
        assert!(m.contains("timeout_secs"), "{m}");
        let m = invalid("[[search.provider]]\nname = \"Nyaa\"\nbogus = 1\n");
        assert!(m.contains("Nyaa") && m.contains("bogus"), "{m}");
        let m =
            invalid("[[search.provider]]\nname = \"Nyaa\"\n[[search.provider]]\nname = \"NYAA\"\n");
        assert!(m.contains("twice"), "{m}");
        let m = invalid(
            "[[search.provider]]\nname = \"Nyaa\"\nkind = \"torznab\"\nurl = \"http://x/\"\n",
        );
        assert!(m.contains("kind"), "{m}");
        let m = invalid("[[search.provider]]\nname = \"x\"\nkind = \"feed\"\nurl = \"ftp://x/\"\n");
        assert!(m.contains("http"), "{m}");
    }

    #[test]
    fn tracker_proxy_and_history_validation() {
        for bad in [
            "[search.tracker_list]\nurl = \"ftp://x/list\"\n",
            "[search.tracker_list]\nrefresh_hours = 0\n",
            "[search]\nproxy = \"nonsense\"\n",
            "[search]\nproxy = \"ftp://h:1\"\n",
            "[search]\nhistory_size = 100000\n",
        ] {
            assert!(parse(bad).validate().is_err(), "{bad}");
        }
        let c = parse(
            "[search]\nproxy = \"socks5h://127.0.0.1:9050\"\nhistory_size = 0\n[search.tracker_list]\nenabled = true\nrefresh_hours = 6\nattach_to_new = false\n",
        );
        c.validate().unwrap();
        assert!(c.search.tracker_list.enabled && !c.search.tracker_list.attach_to_new);
        assert_eq!(
            c.search.tracker_list.url,
            dman_search::DEFAULT_TRACKER_LIST_URL
        );
    }

    #[test]
    fn provider_proxy_and_mirrors_are_overridable() {
        let c = parse(
            "[search]\nproxy = \"socks5h://u:pw@127.0.0.1:9050\"\n\
             [[search.provider]]\nname = \"Nyaa\"\nproxy = \"none\"\nmirrors = [\"https://nyaa.land\", \"https://nyaa.example:8443\"]\n\
             [[search.provider]]\nname = \"YTS\"\nproxy = \"http://proxy.invalid:3128\"\n",
        );
        c.validate().unwrap();
        let defs = c.search.effective_providers().unwrap();
        let nyaa = defs.iter().find(|d| d.name == "Nyaa").unwrap();
        assert!(nyaa.proxy.as_ref().unwrap().is_direct());
        assert_eq!(nyaa.mirrors.len(), 2);
        let yts = defs.iter().find(|d| d.name == "YTS").unwrap();
        assert_eq!(
            yts.proxy.as_ref().unwrap().as_str(),
            "http://proxy.invalid:3128"
        );
        // built-in mirrors survive an override that does not mention them
        assert!(!yts.mirrors.is_empty());
        let text = toml::to_string(&c).unwrap();
        assert_eq!(parse(&text), c);
    }

    #[test]
    fn bad_provider_proxy_and_mirrors_are_rejected_without_leaking_secrets() {
        let m =
            invalid("[[search.provider]]\nname = \"Nyaa\"\nproxy = \"ftp://user:topsecret@h:1\"\n");
        assert!(m.contains("Nyaa") && m.contains("proxy"), "{m}");
        assert!(!m.contains("topsecret"), "{m}");
        let m = invalid(
            "[[search.provider]]\nname = \"Nyaa\"\nmirrors = [\"https://a.example/path\"]\n",
        );
        assert!(m.contains("mirrors"), "{m}");
        let m = invalid("[[search.provider]]\nname = \"Nyaa\"\nmirrors = [\"nonsense\"]\n");
        assert!(m.contains("mirrors"), "{m}");
        let m = invalid("[search]\nproxy = \"ftp://user:topsecret@h:1\"\n");
        assert!(
            m.contains("search.proxy") && !m.contains("topsecret"),
            "{m}"
        );
    }

    #[test]
    fn dns_setting_is_validated() {
        let c = parse("[search]\ndns = \"doh:quad9\"\n");
        c.validate().unwrap();
        assert_eq!(Config::default().search.dns, "system");
        for bad in ["doh:", "doh:http://x/q", "8.8.8.8"] {
            let m = invalid(&format!("[search]\ndns = \"{bad}\"\n"));
            assert!(m.contains("search.dns"), "{m}");
        }
    }

    #[test]
    fn debug_output_hides_proxy_credentials() {
        let c = parse("[search]\nproxy = \"socks5h://alice:hunter2@127.0.0.1:9050\"\n");
        let dbg = format!("{c:?} {:?}", c.search);
        assert!(!dbg.contains("hunter2") && !dbg.contains("alice"), "{dbg}");
        assert!(dbg.contains("127.0.0.1:9050"));
    }

    #[test]
    fn unknown_search_keys_rejected() {
        assert!(
            Config::from_toml_str("[search]\nbogus = 1\n", std::path::Path::new("c.toml")).is_err()
        );
    }
}
