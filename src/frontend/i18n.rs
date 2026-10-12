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

/// Flat lookup map: "appbar.new_entry" -> "新建 Entry".
type FlatMap = HashMap<String, String>;

fn build_flat(json: &str) -> FlatMap {
    // 走 `serde_json::Value` 自己手剥：`_meta` 与其它顶层 section 平级，
    // 不能同时用 `#[serde(flatten)]`（rust 派生会在两个 flatten 字段间
    // 产生冲突，要么一个也起不到作用，要么在某个非 string 值上报
    // "invalid type: map, expected a string"）。手剥只取 string leaf。
    let parsed: serde_json::Value = serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("locale JSON malformed: {e}"));
    let mut out = FlatMap::with_capacity(64);
    let Some(root) = parsed.as_object() else {
        panic!("locale JSON malformed: top-level must be an object");
    };
    for (section, kvs) in root {
        if section == "_meta" { continue; }
        let Some(kvs) = kvs.as_object() else { continue; };
        for (k, v) in kvs {
            let Some(s) = v.as_str() else { continue; };
            out.insert(format!("{section}.{k}"), s.to_string());
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
    ///
    /// Reads `locale` via `get()` (not `get_untracked()`) so every `move || i18n.t(...)`
    /// site subscribes to the locale signal and re-renders when the user picks a new
    /// language. Locale changes are rare (only on language-button click), so the
    /// extra tracking cost is negligible compared to the bug of stale strings.
    pub fn t(&self, id: &str) -> String {
        let loc = self.locale.get();
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
        #[cfg(target_arch = "wasm32")]
        persist_locale(loc);
    }
}

/// 把用户最近的显式选择写回 localStorage + cookie + 当前文档的 `<html lang/dir>`。
///
/// 三处一起更新，避免下次 SSR（读 cookie）和下次 head 内联脚本（读 localStorage）
/// 退回 SSR 的兜底决议。`localStorage.set_item` 在隐私模式下会抛
/// `SecurityError`，这里吞错——和 `agent_panel.rs::save_panel_layout` 同款，
/// 下次刷新退化到 SSR 默认值，不会让面板/语言切死。
///
/// cookie setter 在 web-sys 里挂在 `HtmlDocument` 上，`Window::document()` 返回
/// 的 `Document` 类型本身没有——`dyn_into` 一次把接口对齐过去。`lang` / `dir` 改
/// `documentElement`（`Element`），那个接口就在 `Document` 上，不需要 cast。
#[cfg(target_arch = "wasm32")]
fn persist_locale(loc: Locale) {
    use wasm_bindgen::JsCast;
    let Some(window) = web_sys::window() else { return };
    if let Ok(Some(storage)) = window.local_storage() {
        let _ = storage.set_item("rodeo_lang", loc.as_str());
    }
    let Some(document) = window.document() else { return };
    if let Some(root) = document.document_element() {
        let _ = root.set_attribute("lang", loc.as_str());
        let _ = root.set_attribute(
            "dir",
            if loc.is_rtl() { "rtl" } else { "ltr" },
        );
    }
    // 写 cookie：cast 到 HtmlDocument。cast 失败（极端环境）就静默退回，
    // 下次 SSR 还能从 localStorage 落回 head 脚本里。
    if let Ok(html_doc) = document.dyn_into::<web_sys::HtmlDocument>() {
        let cookie = format!(
            "rodeo_lang={}; path=/; max-age=31536000; samesite=lax",
            loc.as_str()
        );
        let _ = html_doc.set_cookie(&cookie);
    }
}

pub fn provide_i18n(initial: Locale) -> I18nController {
    let ctrl = I18nController { locale: RwSignal::new(initial) };
    provide_context(ctrl);
    // On hydrate, force the controller signal to match what the head script
    // actually set on `<html lang>`. The head script reads localStorage and
    // may override `<html lang>` away from the SSR-resolved value (e.g.
    // cookie=en + localStorage=zh → SSR rendered English, head script flipped
    // `<html lang>` to "zh"). Without this, the body stays in the SSR locale
    // because Leptos hydrate doesn't trigger a re-render when a signal's
    // initial value differs from the SSR value — the change must go through
    // the signal's reactive machinery. `set()` is a no-op when detected ==
    // initial, so the cookie-only path doesn't churn. SSR (`cfg(feature="ssr")`)
    // never runs this branch because there is no DOM.
    #[cfg(target_arch = "wasm32")]
    sync_locale_to_html_lang(&ctrl);
    ctrl
}

/// Re-read `<html lang>` (post-head-script) and force the controller
/// to land on it. See `provide_i18n` doc for the rationale.
///
/// Mismatch case (cookie=en + localStorage=zh) lands here with
/// `current == detected` because `resolve_initial_locale` already pulled the
/// post-head-script value into the controller's initial signal. The DOM body
/// still carries the SSR (English) text, but Leptos hydrate won't re-render
/// unless the signal's value actually changes after hydrate finishes. A plain
/// `set(detected)` is a no-op. We schedule a two-step En-bounce in
/// separate macrotasks: first set to En (re-renders body in English), then
/// set to `detected` (re-renders body in `detected`). Each macrotask is its
/// own reactive flush so both writes fire and the second one wins. The
/// intermediate English flash is unavoidable in the SSR/hydrate mismatch
/// case but only happens once per page-load on a stale-localStorage tab.
#[cfg(target_arch = "wasm32")]
fn sync_locale_to_html_lang(ctrl: &I18nController) {
    let Some(window) = web_sys::window() else { return };
    let Some(document) = window.document() else { return };
    let Some(root) = document.document_element() else { return };
    let Some(lang) = root.get_attribute("lang") else { return };
    let Some(detected) = Locale::from_browser(&lang) else { return };
    let current = ctrl.locale.get_untracked();
    if current != detected {
        // Genuine mismatch: the initial signal is wrong. Force it now so
        // the body re-renders. (Plain Leptos hydration skipped this update
        // because the SSR DOM was rendered against a *different* locale
        // signal — cookie-driven SSR vs localStorage-driven hydrate.)
        ctrl.locale.set(detected);
        return;
    }
    // current == detected: hydrate's reactive system collapses the
    // initial-signal-write. Schedule a two-tick En→detected re-apply via
    // `setTimeout(0)`. Each macrotask is its own reactive flush, so each
    // `set` fires its own re-render: first back to English (matching the
    // SSR DOM), then back to `detected` (what the head script chose).
    schedule_en_bounce(&window, ctrl, detected);
}

#[cfg(target_arch = "wasm32")]
#[inline(never)]
fn schedule_en_bounce(window: &web_sys::Window, ctrl: &I18nController, detected: Locale) {
    use wasm_bindgen::JsCast;
    let ctrl_a = *ctrl;
    let cb_a = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
        ctrl_a.locale.set(Locale::En);
        let ctrl_b = ctrl_a;
        let window_b = web_sys::window();
        let cb_b = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
            ctrl_b.locale.set(detected);
        }) as Box<dyn FnMut()>);
        if let Some(w) = window_b {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb_b.as_ref().unchecked_ref(),
                0,
            );
        }
        cb_b.forget();
    }) as Box<dyn FnMut()>);
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        cb_a.as_ref().unchecked_ref(),
        0,
    );
    cb_a.forget();
}

/// 静态兜底控制器：`generate_route_list(App)` 在启动期会走一遍 App 子树，
/// 那时 ShellInner 还没运行（它是按请求包裹的），所以 `use_context` 拿不到
/// 真正的 controller。给一个全局默认（En）让这次冷启动扫描不 panic。
/// 真正渲染时 ShellInner 提供的 controller 会在最近的作用域里覆盖它。
fn fallback_controller() -> I18nController {
    static CTRL: OnceLock<I18nController> = OnceLock::new();
    *CTRL.get_or_init(|| I18nController { locale: RwSignal::new(Locale::En) })
}

pub fn use_i18n() -> I18nController {
    use_context::<I18nController>().unwrap_or_else(fallback_controller)
}
