//! Key bindings: contexts, defaults, config overrides and key labels.
//!
//! Keys are [`crokey::KeyCombination`]s, normalised by [`normalize`] so that the
//! same physical key matches regardless of how the terminal reports SHIFT
//! (`?` arrives as `?` or `shift-?`, `P` as `P` with or without SHIFT).
//!
//! Lookup goes from the most specific context to [`Context::Global`]: a key bound
//! in the current view's context shadows the same key in the global context.

use std::collections::BTreeMap;

use crokey::KeyCombination;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Context {
    /// Everywhere (unless shadowed by a more specific context).
    Global,
    /// The downloads list.
    Downloads,
    /// The detail view.
    Detail,
    /// The help screen.
    Help,
    /// The add view.
    Add,
    /// The search view.
    Search,
    /// The search providers settings.
    Providers,
}

impl Context {
    /// All contexts, in help-screen order.
    pub const ALL: &'static [Context] = &[
        Context::Global,
        Context::Downloads,
        Context::Detail,
        Context::Help,
        Context::Add,
        Context::Search,
        Context::Providers,
    ];

    /// Name used in `config.toml` (`[keys.help] back = ["esc"]`) and on screen.
    pub const fn id(self) -> &'static str {
        match self {
            Context::Global => "global",
            Context::Downloads => "downloads",
            Context::Detail => "detail",
            Context::Help => "help",
            Context::Add => "add",
            Context::Search => "search",
            Context::Providers => "providers",
        }
    }

    fn from_id(id: &str) -> Option<Context> {
        Context::ALL.iter().copied().find(|c| c.id() == id)
    }
}

/// One key bound to one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Normalised key.
    pub key: KeyCombination,
    /// What it does.
    pub action: Action,
}

/// All bindings, per context.
#[derive(Debug, Clone, PartialEq)]
pub struct Keymap {
    contexts: BTreeMap<Context, Vec<Binding>>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::defaults()
    }
}

/// `(context, action, keys)` default table.
const DEFAULTS: &[(Context, Action, &[&str])] = &[
    (Context::Global, Action::Palette, &[":", "ctrl-p"]),
    (Context::Global, Action::Help, &["?", "f1"]),
    (Context::Global, Action::Quit, &["q", "ctrl-c"]),
    (Context::Global, Action::ReloadConfig, &["f5"]),
    (Context::Global, Action::VideoQualityToggle, &["f6"]),
    (Context::Global, Action::VideoDefaultQuality, &["f7"]),
    (Context::Global, Action::VideoDefaultAudio, &["f8"]),
    (Context::Global, Action::Shutdown, &["Q"]),
    (Context::Downloads, Action::Up, &["k", "up"]),
    (Context::Downloads, Action::Down, &["j", "down"]),
    (Context::Downloads, Action::PageUp, &["pageup", "ctrl-u"]),
    (
        Context::Downloads,
        Action::PageDown,
        &["pagedown", "ctrl-d"],
    ),
    (Context::Downloads, Action::Top, &["g", "home"]),
    (Context::Downloads, Action::Bottom, &["G", "end"]),
    (Context::Downloads, Action::ToggleSelect, &["space"]),
    (Context::Downloads, Action::SelectAll, &["ctrl-a"]),
    (Context::Downloads, Action::SelectNone, &["esc", "u"]),
    (Context::Downloads, Action::SelectRange, &["v"]),
    (Context::Downloads, Action::SortNext, &["s"]),
    (Context::Downloads, Action::SortReverse, &["S"]),
    (Context::Downloads, Action::Filter, &["/"]),
    (Context::Downloads, Action::ClearFilter, &["ctrl-l"]),
    (Context::Downloads, Action::StoppedNextPage, &["]"]),
    (Context::Downloads, Action::StoppedPrevPage, &["["]),
    (Context::Downloads, Action::Pause, &["p"]),
    (Context::Downloads, Action::Resume, &["r"]),
    (Context::Downloads, Action::PauseAll, &["P"]),
    (Context::Downloads, Action::ResumeAll, &["R"]),
    (Context::Downloads, Action::Remove, &["d", "delete"]),
    (Context::Downloads, Action::RemoveDelete, &["D"]),
    (Context::Downloads, Action::Purge, &["x"]),
    (Context::Downloads, Action::Rename, &["n"]),
    (Context::Downloads, Action::CleanupUnselected, &["C"]),
    (Context::Downloads, Action::ToggleGraph, &["w"]),
    (Context::Downloads, Action::ToggleRemoved, &["H"]),
    (Context::Downloads, Action::MoveUp, &["K"]),
    (Context::Downloads, Action::MoveDown, &["J"]),
    (Context::Downloads, Action::MoveTop, &["T"]),
    (Context::Downloads, Action::MoveBottom, &["B"]),
    (Context::Downloads, Action::LimitUp, &["+", "="]),
    (Context::Downloads, Action::LimitDown, &["-"]),
    (Context::Downloads, Action::SetMaxConcurrent, &["c"]),
    (Context::Downloads, Action::OpenDetail, &["enter", "l"]),
    (Context::Downloads, Action::Add, &["a"]),
    (Context::Downloads, Action::Search, &["f"]),
    (
        Context::Detail,
        Action::Back,
        &["esc", "q", "h", "backspace", "left"],
    ),
    (Context::Detail, Action::Up, &["k", "up"]),
    (Context::Detail, Action::Down, &["j", "down"]),
    (Context::Detail, Action::PageUp, &["pageup", "ctrl-u"]),
    (Context::Detail, Action::PageDown, &["pagedown", "ctrl-d"]),
    (Context::Detail, Action::Top, &["g", "home"]),
    (Context::Detail, Action::Bottom, &["G", "end"]),
    (Context::Detail, Action::DetailNextTab, &["tab"]),
    (Context::Detail, Action::DetailPrevTab, &["backtab"]),
    (Context::Detail, Action::DetailFiles, &["1"]),
    (Context::Detail, Action::DetailPeers, &["2"]),
    (Context::Detail, Action::DetailTrackers, &["3"]),
    (Context::Detail, Action::DetailPieces, &["4"]),
    (Context::Detail, Action::DetailSpeed, &["5"]),
    (Context::Detail, Action::DetailOptions, &["6"]),
    (Context::Detail, Action::DetailToggleFile, &["space"]),
    (Context::Detail, Action::DetailToggleAllFiles, &["a"]),
    (Context::Detail, Action::DetailEditOption, &["enter", "e"]),
    (Context::Detail, Action::Pause, &["p"]),
    (Context::Detail, Action::Resume, &["r"]),
    (Context::Detail, Action::Remove, &["d", "delete"]),
    (Context::Detail, Action::RemoveDelete, &["D"]),
    (Context::Detail, Action::Rename, &["n"]),
    (Context::Help, Action::Up, &["k", "up"]),
    (Context::Help, Action::Down, &["j", "down"]),
    (Context::Help, Action::PageUp, &["pageup", "ctrl-u"]),
    (
        Context::Help,
        Action::PageDown,
        &["pagedown", "ctrl-d", "space"],
    ),
    (Context::Help, Action::Top, &["g", "home"]),
    (Context::Help, Action::Bottom, &["G", "end"]),
    (Context::Help, Action::Back, &["esc", "q", "?"]),
    (Context::Add, Action::Back, &["esc"]),
    (Context::Add, Action::AddSubmit, &["enter", "ctrl-s"]),
    (Context::Add, Action::AddNextField, &["tab"]),
    (Context::Add, Action::AddPrevField, &["backtab"]),
    (Context::Add, Action::AddBrowseFiles, &["ctrl-o"]),
    (
        Context::Add,
        Action::AddPasteClipboard,
        &["ctrl-v", "alt-v"],
    ),
    (Context::Add, Action::AddToggleMirror, &["ctrl-t"]),
    (Context::Add, Action::AddPickDir, &["ctrl-f"]),
    (Context::Add, Action::AddToggleVideo, &["ctrl-g"]),
    (Context::Add, Action::AddToggleAdvanced, &["f2", "alt-o"]),
    (Context::Search, Action::Up, &["k", "up"]),
    (Context::Search, Action::Down, &["j", "down"]),
    (Context::Search, Action::PageUp, &["pageup", "ctrl-u"]),
    (Context::Search, Action::PageDown, &["pagedown", "ctrl-d"]),
    (Context::Search, Action::Top, &["g", "home"]),
    (Context::Search, Action::Bottom, &["G", "end"]),
    (Context::Search, Action::ToggleSelect, &["space"]),
    (Context::Search, Action::SelectAll, &["ctrl-a"]),
    (Context::Search, Action::SelectNone, &["u"]),
    (Context::Search, Action::SortNext, &["s"]),
    (Context::Search, Action::SortReverse, &["S"]),
    (Context::Search, Action::Filter, &["/"]),
    (Context::Search, Action::ClearFilter, &["ctrl-l"]),
    (Context::Search, Action::SearchRun, &["ctrl-r", "r"]),
    (Context::Search, Action::SearchEdit, &["i", "f"]),
    (Context::Search, Action::SearchNextField, &["tab"]),
    (Context::Search, Action::SearchPrevField, &["backtab"]),
    (Context::Search, Action::SearchCancel, &["esc"]),
    (Context::Search, Action::SearchAddDirect, &["enter", "A"]),
    (Context::Search, Action::SearchAdd, &["e", "o", "a"]),
    (Context::Search, Action::SearchHistory, &["h", "f3"]),
    (Context::Search, Action::SearchHistoryClear, &["x"]),
    (Context::Search, Action::SearchProviders, &["p", "f2"]),
    (Context::Search, Action::SearchFilterSeeders, &["m"]),
    (Context::Search, Action::SearchFilterSize, &["z"]),
    (Context::Search, Action::SearchFilterSource, &["O"]),
    (Context::Search, Action::Back, &["q"]),
    (Context::Providers, Action::Up, &["k", "up"]),
    (Context::Providers, Action::Down, &["j", "down"]),
    (Context::Providers, Action::PageUp, &["pageup", "ctrl-u"]),
    (
        Context::Providers,
        Action::PageDown,
        &["pagedown", "ctrl-d"],
    ),
    (Context::Providers, Action::Top, &["g", "home"]),
    (Context::Providers, Action::Bottom, &["G", "end"]),
    (
        Context::Providers,
        Action::ProviderToggle,
        &["space", "enter"],
    ),
    (Context::Providers, Action::ProviderTest, &["t"]),
    (Context::Providers, Action::ProviderImport, &["i"]),
    (Context::Providers, Action::ProviderExport, &["e"]),
    (Context::Providers, Action::ProviderRemove, &["D", "delete"]),
    (Context::Providers, Action::MoveUp, &["K", "-"]),
    (Context::Providers, Action::MoveDown, &["J", "+", "="]),
    (
        Context::Providers,
        Action::Back,
        &["esc", "q", "h", "backspace", "left"],
    ),
];

impl Keymap {
    /// The built-in bindings (vim keys and arrows).
    pub fn defaults() -> Self {
        let mut contexts: BTreeMap<Context, Vec<Binding>> = BTreeMap::new();
        for (ctx, action, keys) in DEFAULTS {
            let list = contexts.entry(*ctx).or_default();
            for k in *keys {
                // The defaults are covered by tests; an unparsable one is skipped.
                if let Ok(key) = parse_key(k) {
                    list.push(Binding {
                        key,
                        action: *action,
                    });
                }
            }
        }
        Self { contexts }
    }

    /// Defaults with overrides from the `[keys]` table of `config.toml`.
    ///
    /// Two shapes are accepted and may be mixed:
    /// `action = ["p", "ctrl-s"]` (applies to the action's home context) and
    /// `[keys.<context>] action = [...]`. A string instead of an array is accepted
    /// too. An override replaces the action's keys in that context and takes the
    /// keys away from any other action of the same context. Problems are returned
    /// as human-readable warnings; the offending entry is ignored.
    pub fn from_config(table: Option<&toml::Table>) -> (Self, Vec<String>) {
        let mut map = Self::defaults();
        let mut warnings = Vec::new();
        let Some(table) = table else {
            return (map, warnings);
        };
        for (name, value) in table {
            if let Some(ctx) = Context::from_id(name)
                && let toml::Value::Table(inner) = value
            {
                for (action_name, keys) in inner {
                    map.apply_override(Some(ctx), action_name, keys, &mut warnings);
                }
                continue;
            }
            map.apply_override(None, name, value, &mut warnings);
        }
        (map, warnings)
    }

    fn apply_override(
        &mut self,
        ctx: Option<Context>,
        action_name: &str,
        value: &toml::Value,
        warnings: &mut Vec<String>,
    ) {
        let Some(action) = Action::from_id(action_name) else {
            warnings.push(format!("[keys] unknown action '{action_name}'"));
            return;
        };
        let raw: Vec<&str> = match value {
            toml::Value::String(s) => vec![s.as_str()],
            toml::Value::Array(a) => a.iter().filter_map(toml::Value::as_str).collect(),
            _ => {
                warnings.push(format!("[keys] {action_name}: expected a list of keys"));
                return;
            }
        };
        let mut keys = Vec::new();
        for k in raw {
            match parse_key(k) {
                Ok(key) => keys.push(key),
                Err(e) => warnings.push(format!("[keys] {action_name}: {e}")),
            }
        }
        if keys.is_empty() {
            return;
        }
        self.set(ctx.unwrap_or(action.home_context()), action, &keys);
    }

    /// Replace the keys of `action` in `ctx`, removing those keys from other
    /// actions of the same context.
    pub fn set(&mut self, ctx: Context, action: Action, keys: &[KeyCombination]) {
        let list = self.contexts.entry(ctx).or_default();
        list.retain(|b| b.action != action && !keys.contains(&b.key));
        list.extend(keys.iter().map(|&key| Binding { key, action }));
    }

    /// The action bound to `key` in `ctx`, falling back to the global context.
    pub fn resolve(&self, ctx: Context, key: KeyCombination) -> Option<Action> {
        let key = normalize(key);
        let find = |c: Context| {
            self.contexts
                .get(&c)
                .and_then(|l| l.iter().find(|b| b.key == key))
                .map(|b| b.action)
        };
        find(ctx).or_else(|| find(Context::Global))
    }

    /// The action bound to a crossterm key event.
    pub fn resolve_event(&self, ctx: Context, ev: KeyEvent) -> Option<Action> {
        self.resolve(ctx, KeyCombination::from(ev))
    }

    /// Keys of `action` in `ctx` (not including the global fallback).
    pub fn keys(&self, ctx: Context, action: Action) -> Vec<KeyCombination> {
        self.contexts
            .get(&ctx)
            .map(|l| {
                l.iter()
                    .filter(|b| b.action == action)
                    .map(|b| b.key)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Keys of `action` in `ctx`, else in the global context.
    pub fn keys_with_global(&self, ctx: Context, action: Action) -> Vec<KeyCombination> {
        let keys = self.keys(ctx, action);
        if keys.is_empty() {
            self.keys(Context::Global, action)
        } else {
            keys
        }
    }

    /// `"p, Ctrl-s"`; empty when unbound.
    pub fn label(&self, ctx: Context, action: Action) -> String {
        self.keys_with_global(ctx, action)
            .into_iter()
            .map(key_label)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The bindings of one context, in definition order.
    pub fn bindings(&self, ctx: Context) -> &[Binding] {
        self.contexts.get(&ctx).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Parse a key as written in `config.toml`: crokey syntax (`ctrl-s`, `pagedown`,
/// `f1`, `space`), plus a bare upper-case letter meaning shift + letter.
///
/// # Errors
/// A message naming the unparsable key.
pub fn parse_key(raw: &str) -> Result<KeyCombination, String> {
    let mut chars = raw.chars();
    let combo = match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_uppercase() => {
            KeyCombination::one_key(KeyCode::Char(c), KeyModifiers::SHIFT)
        }
        _ => crokey::parse(raw).map_err(|e| format!("invalid key '{raw}': {e}"))?,
    };
    if !combo.is_ansi_compatible() {
        return Err(format!(
            "invalid key '{raw}': only single keys are supported"
        ));
    }
    Ok(normalize(combo))
}

/// Canonical form: letters carry SHIFT exactly when upper-case; other printable
/// characters never carry SHIFT (terminals disagree about reporting it).
pub fn normalize(k: KeyCombination) -> KeyCombination {
    let mut mods = k.modifiers;
    let code = match *k.codes.first() {
        KeyCode::Char(c) if c.is_ascii_alphabetic() => {
            if c.is_ascii_uppercase() || mods.contains(KeyModifiers::SHIFT) {
                mods.insert(KeyModifiers::SHIFT);
                KeyCode::Char(c.to_ascii_uppercase())
            } else {
                KeyCode::Char(c)
            }
        }
        KeyCode::Char(c) => {
            mods.remove(KeyModifiers::SHIFT);
            KeyCode::Char(c)
        }
        other => other,
    };
    KeyCombination::one_key(code, mods)
}

/// Compact label: `p`, `P`, `Ctrl-p`, `↑`, `PgDn`, `Space`, `Enter`.
pub fn key_label(k: KeyCombination) -> String {
    let mut out = String::new();
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        out.push_str("Ctrl-");
    }
    if k.modifiers.contains(KeyModifiers::ALT) {
        out.push_str("Alt-");
    }
    let code = *k.codes.first();
    let letter = matches!(code, KeyCode::Char(c) if c.is_ascii_alphabetic());
    if k.modifiers.contains(KeyModifiers::SHIFT) && !letter && code != KeyCode::BackTab {
        out.push_str("Shift-");
    }
    let name = match code {
        KeyCode::Char(' ') => "Space".to_owned(),
        KeyCode::Char(c) if k.modifiers.contains(KeyModifiers::CONTROL) => {
            c.to_ascii_lowercase().to_string()
        }
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Up => "↑".into(),
        KeyCode::Down => "↓".into(),
        KeyCode::Left => "←".into(),
        KeyCode::Right => "→".into(),
        KeyCode::PageUp => "PgUp".into(),
        KeyCode::PageDown => "PgDn".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Backspace => "Bksp".into(),
        KeyCode::Delete => "Del".into(),
        KeyCode::Insert => "Ins".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "Shift-Tab".into(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}"),
    };
    out.push_str(&name);
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn every_default_parses() {
        for (_, action, keys) in DEFAULTS {
            for k in *keys {
                assert!(parse_key(k).is_ok(), "{action:?}: {k}");
            }
        }
    }

    #[test]
    fn every_action_has_a_default_binding() {
        let map = Keymap::defaults();
        for a in Action::ALL {
            let bound = Context::ALL.iter().any(|c| !map.keys(*c, *a).is_empty());
            assert!(bound, "{a:?} has no binding");
            assert!(
                !map.keys(a.home_context(), *a).is_empty(),
                "{a:?} not bound in its home context"
            );
        }
    }

    #[test]
    fn no_conflicts_within_a_context() {
        let map = Keymap::defaults();
        for ctx in Context::ALL {
            let mut seen = HashSet::new();
            for b in map.bindings(*ctx) {
                assert!(
                    seen.insert(b.key),
                    "{ctx:?}: {} bound twice",
                    key_label(b.key)
                );
            }
        }
    }

    #[test]
    fn resolves_vim_and_arrow_keys() {
        let map = Keymap::defaults();
        let d = Context::Downloads;
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(
            map.resolve_event(d, ev(KeyCode::Char('j'), none)),
            Some(Action::Down)
        );
        assert_eq!(
            map.resolve_event(d, ev(KeyCode::Down, none)),
            Some(Action::Down)
        );
        assert_eq!(
            map.resolve_event(d, ev(KeyCode::Char('G'), shift)),
            Some(Action::Bottom)
        );
        // Some terminals send upper-case letters without SHIFT.
        assert_eq!(
            map.resolve_event(d, ev(KeyCode::Char('P'), none)),
            Some(Action::PauseAll)
        );
        assert_eq!(
            map.resolve_event(d, ev(KeyCode::Char('p'), none)),
            Some(Action::Pause)
        );
        // `?` with or without SHIFT.
        assert_eq!(
            map.resolve_event(d, ev(KeyCode::Char('?'), shift)),
            Some(Action::Help)
        );
        assert_eq!(
            map.resolve_event(d, ev(KeyCode::Char('?'), none)),
            Some(Action::Help)
        );
        let ctrl_p = ev(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(map.resolve_event(d, ctrl_p), Some(Action::Palette));
        assert_eq!(
            map.resolve_event(d, ev(KeyCode::Enter, none)),
            Some(Action::OpenDetail)
        );
    }

    #[test]
    fn context_shadows_global() {
        let map = Keymap::defaults();
        let q = ev(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(map.resolve_event(Context::Downloads, q), Some(Action::Quit));
        assert_eq!(map.resolve_event(Context::Help, q), Some(Action::Back));
    }

    #[test]
    fn config_overrides() {
        let table: toml::Table = toml::from_str(
            r#"
            pause = ["p", "ctrl-s"]
            resume = "y"
            bogus = ["z"]
            remove = ["not-a-key-at-all"]
            [help]
            back = ["x"]
            "#,
        )
        .unwrap_or_default();
        let (map, warnings) = Keymap::from_config(Some(&table));
        let d = Context::Downloads;
        let ctrl_s = ev(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(map.resolve_event(d, ctrl_s), Some(Action::Pause));
        let y = ev(KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(map.resolve_event(d, y), Some(Action::Resume));
        // `r` no longer resumes.
        let r = ev(KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(map.resolve_event(d, r), None);
        let x = ev(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(map.resolve_event(Context::Help, x), Some(Action::Back));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        // The failed override keeps the defaults.
        assert_eq!(map.label(d, Action::Remove), "d, Del");
    }

    #[test]
    fn override_steals_key_from_other_action() {
        let table: toml::Table = toml::from_str(r#"pause = ["r"]"#).unwrap_or_default();
        let (map, _) = Keymap::from_config(Some(&table));
        let r = ev(KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(
            map.resolve_event(Context::Downloads, r),
            Some(Action::Pause)
        );
        assert!(map.keys(Context::Downloads, Action::Resume).is_empty());
    }

    #[test]
    fn labels() {
        let map = Keymap::defaults();
        assert_eq!(map.label(Context::Downloads, Action::PauseAll), "P");
        assert_eq!(map.label(Context::Downloads, Action::Palette), ":, Ctrl-p");
        assert_eq!(map.label(Context::Downloads, Action::Up), "k, ↑");
        assert_eq!(map.label(Context::Downloads, Action::ToggleSelect), "Space");
    }
}
