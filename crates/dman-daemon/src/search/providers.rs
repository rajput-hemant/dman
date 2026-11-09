//! `providers.*`: listing, changing (applied at once and saved to
//! `config.toml`), testing, importing and exporting provider definitions.

use std::sync::Arc;

use dman_config::{Config, ConfigDoc, Secrets};
use dman_proto::search::{
    ProviderInfo, ProviderNameParams, ProviderSetParams, ProviderTestReport, ProvidersExportParams,
    ProvidersImportParams, ProvidersImported,
};
use dman_proto::{ErrorCode, RpcError};
use dman_search::{Kind, ProviderDef, Searcher, builtin_providers};
use secrecy::ExposeSecret as _;

use super::{SearchService, dto};

/// Hands the API keys referenced by Torznab providers to the searcher.
pub(super) fn apply_secrets(searcher: &Searcher, secrets: &Secrets, defs: &[ProviderDef]) {
    for d in defs {
        if let Kind::Torznab {
            api_key_ref: Some(r),
            ..
        } = &d.kind
        {
            match secrets.get(r) {
                Some(v) => searcher.set_secret(r, v.expose_secret()),
                None if d.enabled => tracing::warn!(
                    provider = %d.name,
                    "secret `{r}` (api_key_ref) is not defined in secrets.toml"
                ),
                None => {}
            }
        }
    }
}

fn is_builtin(name: &str) -> bool {
    builtin_providers()
        .iter()
        .any(|b| b.name.eq_ignore_ascii_case(name))
}

/// URL or template of a definition without userinfo, query and fragment.
fn safe_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
                .replace("%7Bquery%7D", "{query}")
                .replace("%7Bcategory%7D", "{category}")
        }
        Err(_) => raw.split(['?', '#']).next().unwrap_or_default().to_owned(),
    }
}

fn info(d: &ProviderDef) -> ProviderInfo {
    let (url, api_key_ref) = match &d.kind {
        Kind::Torznab {
            url, api_key_ref, ..
        } => (url.as_str(), api_key_ref.clone()),
        Kind::Feed { url } => (url.as_str(), None),
        Kind::Html(h) => (h.search_url.as_str(), None),
        Kind::Json(j) => (j.url.as_str(), None),
    };
    ProviderInfo {
        name: d.name.clone(),
        kind: d.kind.label().to_owned(),
        enabled: d.enabled,
        order: d.order,
        timeout_secs: d.timeout_secs,
        verified: d.verified,
        builtin: is_builtin(&d.name),
        url: safe_url(url),
        description: d.description.clone(),
        rate_limit_per_min: d.rate_limit_per_min,
        max_results: d.max_results,
        api_key_ref,
    }
}

fn config_err(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::General, format!("config.toml: {e}"))
}

impl SearchService {
    fn find(&self, name: &str) -> Result<ProviderDef, RpcError> {
        self.searcher()
            .providers()
            .into_iter()
            .find(|d| d.name.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| RpcError::new(ErrorCode::NotFound, format!("unknown provider `{name}`")))
    }

    pub(super) fn providers_list(&self) -> Vec<ProviderInfo> {
        self.searcher().providers().iter().map(info).collect()
    }

    /// Edits `config.toml` with `edit`, validates the result, saves it and
    /// applies the new provider set to the running searcher.
    async fn change_config(
        &self,
        edit: impl FnOnce(&mut ConfigDoc) -> Result<(), RpcError>,
    ) -> Result<(), RpcError> {
        let _guard = self.config_lock.lock().await;
        let mut doc = ConfigDoc::open(&self.paths).map_err(config_err)?;
        edit(&mut doc)?;
        let text = doc.to_string_lossy();
        let cfg = Config::from_toml_str(&text, doc.path()).map_err(config_err)?;
        cfg.validate()
            .map_err(|e| RpcError::invalid(e.to_string()))?;
        let defs = cfg
            .search
            .effective_providers()
            .map_err(RpcError::invalid)?;
        doc.save().map_err(config_err)?;
        match Secrets::load(&self.paths) {
            Ok(s) => apply_secrets(&self.searcher(), &s, &defs),
            Err(e) => tracing::warn!("cannot reload secrets: {e}"),
        }
        self.searcher().set_providers(defs);
        Ok(())
    }

    pub(super) async fn providers_set(
        &self,
        p: ProviderSetParams,
    ) -> Result<ProviderInfo, RpcError> {
        let def = self.find(&p.name)?;
        let mut fields = toml::Table::new();
        if let Some(v) = p.enabled {
            fields.insert("enabled".into(), v.into());
        }
        if let Some(v) = p.order {
            fields.insert("order".into(), i64::from(v).into());
        }
        if let Some(v) = p.timeout_secs {
            let v = i64::try_from(v).map_err(|_| RpcError::invalid("timeout_secs too large"))?;
            fields.insert("timeout_secs".into(), v.into());
        }
        if fields.is_empty() {
            return Err(RpcError::invalid(
                "nothing to change: give enabled, order or timeout_secs",
            ));
        }
        let name = def.name.clone();
        self.change_config(|doc| {
            doc.patch_search_provider(&name, &fields)
                .map_err(config_err)
        })
        .await?;
        Ok(info(&self.find(&name)?))
    }

    pub(super) async fn providers_test(
        &self,
        p: ProviderNameParams,
    ) -> Result<ProviderTestReport, RpcError> {
        let def = self.find(&p.name)?;
        let start = std::time::Instant::now();
        Ok(match self.searcher().test_provider(&def).await {
            Ok(r) => ProviderTestReport {
                name: def.name,
                ok: true,
                error: None,
                results: u32::try_from(r.results).unwrap_or(u32::MAX),
                sample: r.sample.map(dto),
                elapsed_ms: u64::try_from(r.elapsed.as_millis()).unwrap_or(u64::MAX),
            },
            Err(e) => ProviderTestReport {
                name: def.name,
                ok: false,
                error: Some(e.to_string()),
                results: 0,
                sample: None,
                elapsed_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            },
        })
    }

    /// Adds or replaces custom providers from an exported file. Imported
    /// providers start disabled: nothing is requested until the user enables
    /// them.
    pub(super) async fn providers_import(
        self: &Arc<Self>,
        p: ProvidersImportParams,
    ) -> Result<ProvidersImported, RpcError> {
        let defs = dman_search::import(&p.toml).map_err(|e| RpcError::invalid(e.to_string()))?;
        if defs.is_empty() {
            return Err(RpcError::invalid(
                "the file contains no [[provider]] tables",
            ));
        }
        let mut tables = Vec::new();
        for mut d in defs {
            if is_builtin(&d.name) {
                return Err(RpcError::invalid(format!(
                    "`{}` is the name of a built-in provider; rename it before importing",
                    d.name
                )));
            }
            d.enabled = false;
            d.verified = false;
            tables.push(toml::Table::try_from(&d).map_err(|e| RpcError::invalid(e.to_string()))?);
        }
        let names: Vec<String> = tables
            .iter()
            .filter_map(|t| {
                t.get("name")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        self.change_config(|doc| {
            for t in &tables {
                doc.upsert_search_provider(t).map_err(config_err)?;
            }
            Ok(())
        })
        .await?;
        Ok(ProvidersImported { names })
    }

    /// TOML of the named providers, or of every custom provider.
    pub(super) fn providers_export(&self, p: ProvidersExportParams) -> Result<String, RpcError> {
        let all = self.searcher().providers();
        let chosen: Vec<ProviderDef> = match p.names.filter(|n| !n.is_empty()) {
            Some(names) => {
                let mut out = Vec::new();
                for n in names {
                    out.push(self.find(&n)?);
                }
                out
            }
            None => all.into_iter().filter(|d| !is_builtin(&d.name)).collect(),
        };
        Ok(dman_search::export(&chosen))
    }

    pub(super) async fn providers_remove(&self, p: ProviderNameParams) -> Result<(), RpcError> {
        let def = self.find(&p.name)?;
        if is_builtin(&def.name) {
            return Err(RpcError::new(
                ErrorCode::Refused,
                format!(
                    "`{}` is a built-in provider and cannot be removed; disable it instead",
                    def.name
                ),
            ));
        }
        let name = def.name;
        self.change_config(|doc| {
            doc.remove_search_provider(&name)
                .map(drop)
                .map_err(config_err)
        })
        .await
    }
}
