use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Locale {
    Zh,
    En,
    Ja,
    Pt,
    Es,
    Ar,
}

impl Locale {
    pub fn as_str(self) -> &'static str {
        match self {
            Locale::Zh => "zh",
            Locale::En => "en",
            Locale::Ja => "ja",
            Locale::Pt => "pt",
            Locale::Es => "es",
            Locale::Ar => "ar",
        }
    }
    /// Short label rendered inside the AppBar language button.
    pub fn short_label(self) -> &'static str {
        match self {
            Locale::Zh => "中",
            Locale::En => "EN",
            Locale::Ja => "日",
            Locale::Pt => "PT",
            Locale::Es => "ES",
            Locale::Ar => "ع",
        }
    }
    /// Long label rendered in the language dropdown.
    pub fn label(self) -> &'static str {
        match self {
            Locale::Zh => "中文",
            Locale::En => "English",
            Locale::Ja => "日本語",
            Locale::Pt => "Português",
            Locale::Es => "Español",
            Locale::Ar => "العربية",
        }
    }
    pub fn is_rtl(self) -> bool { matches!(self, Locale::Ar) }

    pub const ALL: [Locale; 6] = [Locale::Zh, Locale::En, Locale::Ja, Locale::Pt, Locale::Es, Locale::Ar];

    pub fn from_str_ci(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "zh" | "zh-cn" | "zh-tw" | "zh-hans" | "zh-hant" => Some(Locale::Zh),
            "en" | "en-us" | "en-gb" => Some(Locale::En),
            "ja" => Some(Locale::Ja),
            "pt" | "pt-br" | "pt-pt" => Some(Locale::Pt),
            "es" | "es-es" | "es-mx" => Some(Locale::Es),
            "ar" => Some(Locale::Ar),
            _ => None,
        }
    }

    /// Reduce any browser-language string ("zh-CN", "zh-Hans-CN", "fr-FR") to a base
    /// language ("zh","zh","fr"), then try `from_str_ci`.
    pub fn from_browser(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() { return None; }
        if let Some(loc) = Self::from_str_ci(s) { return Some(loc); }
        // Strip region / script suffixes.
        let base = s.split(['-', '_']).next().unwrap_or(s);
        Self::from_str_ci(base)
    }

    /// Parse an `Accept-Language` header value
    /// (e.g. `fr-FR;q=0.9,zh-CN;q=0.8,en;q=0.5`) and return the highest-q
    /// entry that maps to one of our six locales. Entries with no `q=` default
    /// to 1.0; `q=0` items are skipped.
    pub fn from_accept_language(header: &str) -> Option<Self> {
        let mut best: Option<(f32, Self)> = None;
        for raw in header.split(',') {
            let item = raw.trim();
            if item.is_empty() { continue; }
            let (tag, q) = match item.split_once(";q=") {
                Some((t, q)) => (t.trim(), q.parse::<f32>().unwrap_or(1.0)),
                None => match item.split_once(';') {
                    Some((t, _)) => (t.trim(), 1.0),
                    None => (item, 1.0),
                },
            };
            if !(q > 0.0) { continue; }
            if let Some(loc) = Self::from_browser(tag) {
                match best {
                    Some((bq, _)) if bq >= q => {}
                    _ => best = Some((q, loc)),
                }
            }
        }
        best.map(|(_, l)| l)
    }
}

#[derive(Deserialize)]
struct LocaleJson {
    #[serde(default)]
    #[allow(dead_code)]
    _meta: HashMap<String, String>,
    #[serde(flatten)]
    sections: HashMap<String, HashMap<String, String>>,
}

/// Flat lookup map: "appbar.new_entry" -> "新建 Entry".
type FlatMap = HashMap<String, String>;

fn build_flat(json: &str) -> FlatMap {
    let parsed: LocaleJson = serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("locale JSON malformed: {e}"));
    let mut out = FlatMap::with_capacity(64);
    for (section, kvs) in parsed.sections {
        for (k, v) in kvs {
            out.insert(format!("{section}.{k}"), v);
        }
    }
    out
}

fn map_for(loc: Locale) -> &'static FlatMap {
    static EN: OnceLock<FlatMap> = OnceLock::new();
    static ZH: OnceLock<FlatMap> = OnceLock::new();
    static JA: OnceLock<FlatMap> = OnceLock::new();
    static PT: OnceLock<FlatMap> = OnceLock::new();
    static ES: OnceLock<FlatMap> = OnceLock::new();
    static AR: OnceLock<FlatMap> = OnceLock::new();
    match loc {
        Locale::En => EN.get_or_init(|| build_flat(include_str!("../../locales/en.json"))),
        Locale::Zh => ZH.get_or_init(|| build_flat(include_str!("../../locales/zh.json"))),
        Locale::Ja => JA.get_or_init(|| build_flat(include_str!("../../locales/ja.json"))),
        Locale::Pt => PT.get_or_init(|| build_flat(include_str!("../../locales/pt.json"))),
        Locale::Es => ES.get_or_init(|| build_flat(include_str!("../../locales/es.json"))),
        Locale::Ar => AR.get_or_init(|| build_flat(include_str!("../../locales/ar.json"))),
    }
}

#[derive(Clone, Copy)]
pub struct I18nController { pub locale: RwSignal<Locale> }

impl I18nController {
    /// Look up an id in the current locale, fall back to en, fall back to the id itself.
    pub fn t(&self, id: &str) -> String {
        let loc = self.locale.get_untracked();
        if let Some(v) = map_for(loc).get(id) { return v.clone(); }
        if let Some(v) = map_for(Locale::En).get(id) { return v.clone(); }
        id.to_string()
    }

    /// Look up with `{name}`-style interpolation. Args is a slice of (placeholder, value).
    pub fn tf(&self, id: &str, args: &[(&str, &str)]) -> String {
        let mut s = self.t(id);
        for (k, v) in args {
            let pat = format!("{{{k}}}");
            s = s.replace(&pat, v);
        }
        s
    }

    pub fn set(&self, loc: Locale) {
        self.locale.set(loc);
    }
}

pub fn provide_i18n(initial: Locale) -> I18nController {
    let ctrl = I18nController { locale: RwSignal::new(initial) };
    provide_context(ctrl);
    ctrl
}

pub fn use_i18n() -> I18nController {
    use_context::<I18nController>().expect("I18nController 未提供")
}
