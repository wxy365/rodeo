//! 主题状态：**设计语言**（经典 / Material）× **亮暗**（亮 / 暗），两轴正交。
//!
//! 四态：`ClassicLight` → `ClassicDark` → `MaterialLight` → `MaterialDark`
//! →（回到 `ClassicLight`）。落到 `<html>` 上是两个正交属性：
//!
//! - `data-theme` = `light` | `dark`
//! - `data-skin`  = `classic` | `material`
//!
//! 两个属性正交是整套方案的地基：CSS 侧「亮暗」和「设计语言」可以各自
//! 独立地写规则，不必把四种组合硬编码进选择器。
//!
//! 原来的第三档 `System`（跟随系统）被移除——四态循环里没有它。存量用户
//! localStorage 里存着的 `"system"` 由 [`ThemeMode::parse`] 返回 `None`，
//! 再由 head 内联脚本按 `prefers-color-scheme` 解析成具体的经典亮/暗并固化
//! 回写，首屏颜色与改动前完全一致（唯一的行为变化是从此不再跟随 OS 昼夜
//! 切换）。
//!
//! 持久化：存到 `localStorage.rodeo_theme`，值是四个具体态之一
//! （`light` / `dark` / `material-light` / `material-dark`）。
//!
//! FOUC 防御：`<html data-theme/data-skin>` 由 `shell()` 里的内联脚本在首屏
//! CSS 解析之前就打好位——那段脚本读 localStorage、解析、写属性，三件事在
//! 同一个同步脚本里完成，所以首屏不会看到「先亮后暗」的闪一下。
//!
//! SSR 期间不读 localStorage——首屏靠内联脚本 + CSS 回退，已能给出正确色；
//! hydrate 后第一帧才把用户偏好同步到 DOM。

use leptos::prelude::*;

use super::i18n::{use_i18n, I18nController};
use super::icons::{ic_moon, ic_sun};

/// 主题模式：设计语言 × 亮暗。
///
/// `ClassicLight` / `ClassicDark` 是改造前的经典 UI；`MaterialLight` /
/// `MaterialDark` 是 Material 3 皮肤。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeMode {
    ClassicLight,
    ClassicDark,
    MaterialLight,
    MaterialDark,
}

impl ThemeMode {
    /// 从字符串解析（localStorage 反序列化）。
    ///
    /// 只认四个具体值；`"system"`（旧档）与任何未知值都返回 `None`，
    /// 交由调用方走「解析系统偏好并固化」的迁移路径。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "light" => Some(ThemeMode::ClassicLight),
            "dark" => Some(ThemeMode::ClassicDark),
            "material-light" => Some(ThemeMode::MaterialLight),
            "material-dark" => Some(ThemeMode::MaterialDark),
            _ => None,
        }
    }

    /// 序列化为字符串（写进 localStorage）。
    pub fn as_str(&self) -> &'static str {
        match self {
            ThemeMode::ClassicLight => "light",
            ThemeMode::ClassicDark => "dark",
            ThemeMode::MaterialLight => "material-light",
            ThemeMode::MaterialDark => "material-dark",
        }
    }

    /// 四态循环的下一档：经典亮 → 经典暗 → Material 亮 → Material 暗 → 经典亮。
    pub fn next(&self) -> Self {
        match self {
            ThemeMode::ClassicLight => ThemeMode::ClassicDark,
            ThemeMode::ClassicDark => ThemeMode::MaterialLight,
            ThemeMode::MaterialLight => ThemeMode::MaterialDark,
            ThemeMode::MaterialDark => ThemeMode::ClassicLight,
        }
    }

    /// 亮暗轴 → `<html data-theme>`。
    pub fn scheme(&self) -> &'static str {
        match self {
            ThemeMode::ClassicLight | ThemeMode::MaterialLight => "light",
            ThemeMode::ClassicDark | ThemeMode::MaterialDark => "dark",
        }
    }

    /// 设计语言轴 → `<html data-skin>`。
    pub fn skin(&self) -> &'static str {
        match self {
            ThemeMode::ClassicLight | ThemeMode::ClassicDark => "classic",
            ThemeMode::MaterialLight | ThemeMode::MaterialDark => "material",
        }
    }

    /// 当前是不是 Material 皮肤。
    pub fn is_material(&self) -> bool {
        matches!(self, ThemeMode::MaterialLight | ThemeMode::MaterialDark)
    }
}

/// 当前主题 + 切换函数。
///
/// `provide_theme` 在 App 层 provide；任何组件通过 `use_theme()` 拿到。
/// 切换函数直接读写 `data-theme` / `data-skin` 属性与 localStorage，并触发重渲。
#[derive(Clone, Copy)]
pub struct ThemeController {
    pub mode: RwSignal<ThemeMode>,
}

impl ThemeController {
    /// 切换到下一档：经典亮 → 经典暗 → Material 亮 → Material 暗。
    pub fn cycle(&self) {
        let next = self.mode.get_untracked().next();
        self.set(next);
    }

    /// 显式设定：写信号 + 写 `<html>` 两个属性 + 写 localStorage。
    /// 三个副作用在同一个 set() 里完成，保证观察者从外部看是原子的——
    /// 否则读到「新 mode + 旧 DOM 属性」的状态会出现几毫秒的错色。
    pub fn set(&self, mode: ThemeMode) {
        apply_to_dom(mode);
        write_storage(mode);
        self.mode.set(mode);
    }
}

pub fn provide_theme() -> ThemeController {
    let ctrl = ThemeController {
        mode: RwSignal::new(read_storage()),
    };
    provide_context(ctrl);
    ctrl
}

pub fn use_theme() -> ThemeController {
    use_context::<ThemeController>().expect("ThemeController 未提供")
}

/// 把当前 mode 推到 `<html>`：`data-theme`（亮暗）与 `data-skin`（设计语言）。
///
/// 两个属性正交，CSS 侧才能把「亮暗」和「设计语言」各自独立地写规则，
/// 不必把四种组合硬编码进选择器。
fn apply_to_dom(mode: ThemeMode) {
    apply_data_attrs(mode.scheme(), mode.skin());
}

#[cfg(target_arch = "wasm32")]
fn apply_data_attrs(scheme: &str, skin: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(doc) = window.document() else { return };
    let Some(html) = doc.document_element() else {
        return;
    };
    html.set_attribute("data-theme", scheme).ok();
    html.set_attribute("data-skin", skin).ok();
}

#[cfg(not(target_arch = "wasm32"))]
fn apply_data_attrs(_scheme: &str, _skin: &str) {
    // SSR 阶段没 DOM 可写；CSS 在 hydrate 时一次性接管即可。
}

#[cfg(target_arch = "wasm32")]
fn write_storage(mode: ThemeMode) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(Some(storage)) = window.local_storage() else {
        return;
    };
    let _ = storage.set_item("rodeo_theme", mode.as_str());
}

#[cfg(not(target_arch = "wasm32"))]
fn write_storage(_mode: ThemeMode) {}

/// 读当前主题。三级兜底：
///
/// 1. `localStorage.rodeo_theme` —— 正常路径，四个具体值之一。
/// 2. `<html>` 上的 `data-theme` / `data-skin` —— head 内联脚本已经按系统
///    偏好解析并写好了属性；localStorage 不可用（隐私模式）时走这条。
/// 3. `ClassicLight` —— 兜底。
///
/// 注意：**不**在此处读 `prefers-color-scheme`。那需要新增 web-sys 的
/// `MediaQueryList` feature，而 head 内联脚本里已经用原生 JS 做完了解析，
/// 没必要让 Rust 再解一遍。
#[cfg(target_arch = "wasm32")]
pub fn read_storage() -> ThemeMode {
    let Some(window) = web_sys::window() else {
        return ThemeMode::ClassicLight;
    };
    if let Ok(Some(storage)) = window.local_storage() {
        if let Ok(Some(s)) = storage.get_item("rodeo_theme") {
            if let Some(mode) = ThemeMode::parse(&s) {
                return mode;
            }
        }
    }
    // localStorage 没有 / 不可用 → 读内联脚本写好的 DOM 属性。
    if let Some(doc) = window.document() {
        if let Some(html) = doc.document_element() {
            let scheme = html.get_attribute("data-theme").unwrap_or_default();
            let skin = html.get_attribute("data-skin").unwrap_or_default();
            let mode = match (skin.as_str(), scheme.as_str()) {
                ("material", "light") => Some(ThemeMode::MaterialLight),
                ("material", "dark") => Some(ThemeMode::MaterialDark),
                ("classic", "light") => Some(ThemeMode::ClassicLight),
                ("classic", "dark") => Some(ThemeMode::ClassicDark),
                _ => None,
            };
            if let Some(m) = mode {
                return m;
            }
        }
    }
    ThemeMode::ClassicLight
}

#[cfg(not(target_arch = "wasm32"))]
pub fn read_storage() -> ThemeMode {
    ThemeMode::ClassicLight
}

/// 主题按钮：四态循环 —— 经典亮 → 经典暗 → Material 亮 → Material 暗。
///
/// **两枚图标永远都渲染在 DOM 里**，显示哪一枚由 CSS 按 `<html data-theme>`
/// 决定（见 `style/main.css` 的 `.theme-btn .ti` 规则）。这样做的直接好处是
/// SSR 与 hydrate 渲染出的 DOM 结构完全一致 —— 原先那种
/// `{match mode.get() { … }.into_any()}` 写法在 SSR（读不到 localStorage）
/// 与 hydrate（读得到）之间会渲染出两棵不同的 SVG 子树，是一处既存的
/// hydration 隐患。
///
/// 图标本身始终是太阳/月亮，只负责表达亮暗；「经典 vs Material」靠**颜色
/// 通道**表达：Material 态下按钮图标染成 `--md-primary`（靛蓝），经典态
/// 保持中性灰。
#[component]
pub fn ThemeButton() -> impl IntoView {
    let i18n: I18nController = use_i18n();
    let ctrl = use_theme();
    let mode = ctrl.mode;

    view! {
        <button class="theme-btn"
            title=move || match mode.get() {
                ThemeMode::ClassicLight => i18n.t("theme.title_classic_light"),
                ThemeMode::ClassicDark => i18n.t("theme.title_classic_dark"),
                ThemeMode::MaterialLight => i18n.t("theme.title_material_light"),
                ThemeMode::MaterialDark => i18n.t("theme.title_material_dark"),
            }
            aria-label=move || i18n.t("theme.aria")
            on:click=move |_| ctrl.cycle()>
            <span class="ti ti-sun">{ic_sun()}</span>
            <span class="ti ti-moon">{ic_moon()}</span>
        </button>
    }
}
