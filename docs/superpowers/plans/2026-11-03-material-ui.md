# Material UI 皮肤 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 Rodeo 加一套 Material 3 风格的年轻活力 UI（靛蓝 + 珊瑚），覆盖全部界面，通过顶栏主题按钮在「经典亮/暗」与「Material 亮/暗」四态间循环切换。

**Architecture:** 两个正交的 `<html>` 属性承载状态——`data-theme`（light/dark）× `data-skin`（classic/material）。视觉差异全部落在 CSS：`style/main.css` 分三层（经典段零改动 → Material 令牌层 → Material 结构覆盖层），Material 段复用现有 token 名字（`--ink`/`--bg`/`--line`…）重新指向 M3 语义，约 70% 界面自动获得 Material 观感。Rust 侧只改主题状态机一处。

**Tech Stack:** Rust + Leptos 0.8（SSR + WASM hydrate）、原生 CSS 自定义属性、`style/main.css`（cargo-leptos `style-file`，内容哈希）、playwright-core 浏览器验证。

**设计依据：** `docs/superpowers/specs/2026-11-03-material-ui-design.md`（已提交 `0ef99c6`）。本计划是它的可执行化，冲突时以设计文档为准。

## Global Constraints

- **分支**：`feature/material-ui`。master 不动。已有一个提交 `0ef99c6 docs: Material UI 皮肤设计文档`。
- **不补 Rust 单元测试**（项目惯例，`feedback-no-backend-unit-tests`）。验证靠 `make check` + `make build` + 浏览器 E2E。
- **E2E 是硬门槛**（`feedback-end-to-end-test-mandatory`）：编译通过 ≠ 完成，必须实际跑起来验证行为。
- **不要用 `leptos_meta` 的注册式 `<Script>`**（`leptos-meta-registered-script-hydration-break`）：head 内脚本必须是 `ShellInner` 的 `<head>` 里的**普通 `<script>` 元素**。
- **不要抢用户的 :3000**：构建一律用 `LEPTOS_SITE_ROOT=target/site-smoke`；冒烟服务用独立 `data_dir` 起在 :3099；用完必须杀掉自己起的服务。
- **手跑二进制必须设 `LEPTOS_HASH_FILES=true` 和 `LEPTOS_OUTPUT_NAME=rodeo`**，否则 `<HashedStylesheet>` 指向不存在的 `/pkg/rodeo.css`。
- **wasm 会被 CompressionLayer 卡死**：debug wasm 约 37MB，gzip 下 >60s。浏览器脚本必须用 `page.route('**/pkg/*.wasm')` 从磁盘 fulfill。
- **只提交自己的改动**：工作树可能混着别的会话的 WIP，提交前用 `git diff --cached --stat` 核对。
- **提交前缀**：`fix` / `feat` / `chore` / `refactor` / `docs` / `style` / `perf` / `build` / `ci`。
- **不引外网字体**：沿用系统字体栈。
- **不加新组件**：不引入 FAB、不改按钮位置、不加抽屉开关。
- **`docs/` 与 `spec/` 在 .gitignore 里**：新增文档需要 `git add -f`。
- **每个任务结束立即提交**。

---

## File Structure

| 文件 | 责任 |
|---|---|
| `style/main.css` | 全部视觉。经典段（现状）→ Material 令牌层 → Material 结构覆盖层。三段都在同一文件（cargo-leptos `style-file` 只支持一个文件，`hash-files=true` 依赖内容哈希）。 |
| `src/frontend/theme.rs` | 四态状态机：枚举、解析、持久化、DOM 属性应用、主题按钮组件。 |
| `src/app.rs` | head 内联脚本（首屏防色闪 + 旧值迁移）。**唯一**改动点，不新增注册式 `<Script>`。 |
| `src/frontend/icons.rs` | **不改**。复用 `ic_sun()` / `ic_moon()`。 |
| `locales/{en,zh,ja,pt,es,ar}.json` | `theme` 段文案。 |
| `scripts/material-theme-e2e.mjs` | 新建。四态循环 / 持久化 / 迁移 / 无 hydration 报错 / 各界面截图 的浏览器验证。 |

---

### Task 1: CSS token 化（经典外观零变化）

**Files:**
- Modify: `style/main.css`（`:root` 段 L1-39；`.ibtn` L226；`.btn` L197；`.inp` L242；`.chip` L287；`.panel` L366；以及 12 处硬编码 `box-shadow`、21 处硬编码 `border-radius`）

**Interfaces:**
- Consumes: 无
- Produces: 供 Task 3/4/5/6 覆写的 token：
  `--sh-1` `--sh-2` `--sh-3` `--h-btn` `--h-ibtn` `--r-pill` `--state-hover` `--state-press`
  以及把写死的阴影/圆角改成 `var(--token, <经典原值>)` 形式。

- [ ] **Step 1: 在 `:root` 里补 token（默认值 = 经典现状）**

在 `style/main.css` 的 `:root { ... }` 块内，`--r-l: 12px;` 那一行之后追加：

```css
    /* ===== 形状 / 层次 / 控件尺寸 token =====
       这批 token 的默认值刻意等于经典 UI 现状：经典路径取值与改动前完全一致，
       所以「token 化」这一步对经典外观零影响。Material 皮肤在令牌层覆写同一批
       token，一次性改掉全站同类元素的形状与层次。

       阴影：经典 UI 的阴影全是写死的散值，这里统一收进 --sh-1/2/3 三档。
       --sh-1 一级浮层（卡片/下拉），--sh-2 二级（菜单/弹窗），--sh-3 三级（对话框/侧边面板）。 */
    --sh-1: none;
    --sh-2: none;
    --sh-3: none;
    /* 按钮与图标按钮的高度。经典靠 padding 撑出尺寸，这里给出显式高度供 Material 覆写。 */
    --h-btn: auto;
    --h-ibtn: 28px;
    /* 胶囊圆角。经典按钮用 --r-s，这里给 Material 的胶囊按钮用。 */
    --r-pill: 6px;
    /* M3 state layer：hover / 按下时叠加的半透明层。经典态完全透明 = 无效果。 */
    --state-hover: transparent;
    --state-press: transparent;
```

- [ ] **Step 2: 把 12 处硬编码 `box-shadow` 换成 `var(--sh-N, <原值>)`**

用下面这张对照表逐处替换（保持选择器与其它属性不变，只改 `box-shadow` 那一行）：

| 行号 | 选择器 | 原值 | 改为 |
|---|---|---|---|
| 970 | `.lblhint` | `0 6px 20px rgb(0 0 0 / 8%)` | `var(--sh-2, 0 6px 20px rgb(0 0 0 / 8%))` |
| 994 | `.timepick` | `0 6px 18px rgb(0 0 0 / 18%)` | `var(--sh-2, 0 6px 18px rgb(0 0 0 / 18%))` |
| 1545 | `.cpick-pop` | `0 6px 18px rgba(15, 23, 42, 0.14)` | `var(--sh-2, 0 6px 18px rgba(15, 23, 42, 0.14))` |
| 1993 | `.avatar-drop` | `0 6px 20px rgb(0 0 0 / 0.12)` | `var(--sh-2, 0 6px 20px rgb(0 0 0 / 0.12))` |
| 2058 | `.msg-panel` | `0 4px 16px rgba(0, 0, 0, 0.12)` | `var(--sh-2, 0 4px 16px rgba(0, 0, 0, 0.12))` |
| 2433 | `.entry-picker-results` | `0 4px 12px rgba(0,0,0,.08)` | `var(--sh-1, 0 4px 12px rgba(0,0,0,.08))` |
| 2584 | `.newentry-btn` | `0 1px 2px rgb(0 0 0 / 12%)` | `var(--sh-1, 0 1px 2px rgb(0 0 0 / 12%))` |
| 2602 | `.newentry-split` | `0 6px 20px rgb(0 0 0 / 12%)` | `var(--sh-2, 0 6px 20px rgb(0 0 0 / 12%))` |
| 2716 | `.agent-panel` | `-6px 0 20px rgb(0 0 0 / 8%), 0 6px 24px rgb(0 0 0 / 10%)` | `var(--sh-3, -6px 0 20px rgb(0 0 0 / 8%), 0 6px 24px rgb(0 0 0 / 10%))` |
| 3460 | `.vmenu` | `0 6px 20px rgb(0 0 0 / 14%)` | `var(--sh-2, 0 6px 20px rgb(0 0 0 / 14%))` |
| 3496 | `.emenu` | `0 6px 20px rgb(0 0 0 / 14%)` | `var(--sh-2, 0 6px 20px rgb(0 0 0 / 14%))` |
| 3592 | `.ws-switch-drop` | `0 6px 20px rgb(0 0 0 / 14%)` | `var(--sh-2, 0 6px 20px rgb(0 0 0 / 14%))` |
| 3662 | `.lang-drop` | `0 6px 20px rgb(0 0 0 / 14%)` | `var(--sh-2, 0 6px 20px rgb(0 0 0 / 14%))` |

**注意** `.agent-panel` 的 `box-shadow` 后面紧跟 `transition: box-shadow 0.15s ease;`（L2722），只改 `box-shadow` 那一行，别动 transition。

- [ ] **Step 3: 把关键控件的写死圆角/尺寸换成 token**

以下几处的圆角是 Material 观感的主要来源，逐处替换：

| 行号 | 选择器 | 原值 | 改为 |
|---|---|---|---|
| 232 | `.ibtn` | `border-radius: 8px;` | `border-radius: var(--r-pill);` |
| 232 | `.ibtn` | `width: 28px; height: 28px;` | `width: var(--h-ibtn); height: var(--h-ibtn);` |
| 722 | `.ws-card .invite-tag` | `border-radius: 4px;` | `border-radius: var(--r-s);` |
| 992 | `.timepick` | `border-radius: 6px;` | `border-radius: var(--r-m);` |
| 1173 | `.lblrow` | `border-radius: 8px;` | `border-radius: var(--r-m);` |
| 1235 | `.pager button` | `border-radius: 8px;` | `border-radius: var(--r-pill);` |
| 1884 | `.switch` | `border-radius: 11px;` | `border-radius: var(--r-pill);` |
| 2057 | `.msg-panel` | `border-radius: 8px;` | `border-radius: var(--r-m);` |
| 2132 | `.colchip` | `border-radius: 8px;` | `border-radius: var(--r-pill);` |

**注意**：`.ibtn` 的 `border-radius` 经典值是 `8px`，而 `--r-pill` 默认是 `6px` —— 所以**必须**同时把 `:root` 里 `--r-pill` 的默认值改成 `8px`，否则 `.ibtn` 的经典外观会从 8px 变成 6px。把 Step 1 里 `--r-pill: 6px;` 改成 `--r-pill: 8px;`。

同理 `.switch` 经典是 11px，`--r-pill` 默认 8px 不匹配 —— `.switch` 单独处理，**不改**（保持 `border-radius: 11px`），Material 覆盖层里再改成胶囊。

- [ ] **Step 4: 验证经典外观没变（静态比对）**

```bash
cd /Users/wangxiaoyan/Works/git/rodeo
git stash list >/dev/null
# 把改动后的 main.css 与 HEAD 版本逐条对比：所有 var() 的回退位必须等于原值
git diff style/main.css | grep -E "^[+-].*(border-radius|box-shadow|width|height)" | head -60
```

Expected: 每一处 `-` 行的原值都能在对应的 `+` 行的 `var(--token, ...)` 回退位里原样找到；没有任何「原值消失且回退位不同」的改动。

- [ ] **Step 5: 类型检查 + 构建**

```bash
make check
```

Expected: native 与 wasm32 两侧都通过（纯 CSS 改动，Rust 不受影响，但按项目门槛跑）。

- [ ] **Step 6: 提交**

```bash
git add style/main.css
git diff --cached --stat
git commit -m "refactor(css): 把圆角/阴影/控件尺寸抽成 token，经典外观不变

Material 皮肤的形状与层次差异主要来自圆角、阴影、控件密度三类值，而它们
在 main.css 里全是写死的散值。这批值先收进 --sh-1/2/3、--h-btn、--h-ibtn、
--r-pill、--state-hover、--state-press 七个 token。

关键约束：token 的默认值刻意等于经典 UI 现状，且引用一律写成
var(--token, <经典原值>)，所以经典路径取值与改动前逐条一致 —— 这一步对
经典外观零影响，Material 皮肤后续只需在令牌层覆写同一批 token。

经典外观的等价性靠 E2E 逐屏截图比对验证（见 material-theme-e2e.mjs）。"
```

---

### Task 2: 主题四态状态机 + i18n

**Files:**
- Modify: `src/frontend/theme.rs`（全文重写枚举与控制器）
- Modify: `src/app.rs:143-145`（head 内联脚本）
- Modify: `locales/{en,zh,ja,pt,es,ar}.json`（`theme` 段）

**Interfaces:**
- Consumes: Task 1 的 `--r-pill` 等 token（仅 CSS 侧）
- Produces:
  - `ThemeMode::{ClassicLight, ClassicDark, MaterialLight, MaterialDark}`
  - `ThemeMode::parse(&str) -> Option<ThemeMode>`、`as_str(&self) -> &'static str`、`next(&self) -> ThemeMode`、`scheme(&self) -> &'static str`、`skin(&self) -> &'static str`、`is_material(&self) -> bool`
  - `ThemeController::cycle(&self)`
  - `<html>` 上的 `data-theme` / `data-skin` 属性

- [ ] **Step 1: 重写 `src/frontend/theme.rs` 的枚举与控制器**

把 `ThemeMode` 整个枚举与 `impl ThemeMode` 替换为：

```rust
/// 主题模式：设计语言 × 亮暗，两条轴正交。
///
/// `ClassicLight` / `ClassicDark` 是改造前的经典 UI；`MaterialLight` /
/// `MaterialDark` 是本轮新增的 Material 3 皮肤。落到 `<html>` 上是两个
/// 正交属性：`data-theme`（light/dark）与 `data-skin`（classic/material）。
///
/// 原来的第三档 `System`（跟随系统）被移除——四态循环里没有它。存量用户
/// localStorage 里存着的 `"system"` 由 `parse` 返回 `None`，再由 head 内联
/// 脚本按 `prefers-color-scheme` 解析成具体的经典亮/暗并固化回写，首屏颜色
/// 与改动前完全一致。
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
    /// 交给调用方走「解析系统偏好并固化」的迁移路径。
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

    pub fn is_material(&self) -> bool {
        matches!(self, ThemeMode::MaterialLight | ThemeMode::MaterialDark)
    }
}
```

`ThemeController` 的 `cycle` 改为：

```rust
    /// 切换到下一档：经典亮 → 经典暗 → Material 亮 → Material 暗 → 经典亮。
    pub fn cycle(&self) {
        let next = self.mode.get_untracked().next();
        self.set(next);
    }
```

`apply_to_dom` 改为同时写两个属性：

```rust
/// 把当前 mode 推到 `<html>`：`data-theme`（亮暗）与 `data-skin`（设计语言）。
///
/// 两个属性正交，CSS 侧才能把「亮暗」和「设计语言」各自独立地写规则，
/// 不必把四种组合硬编码进选择器。
fn apply_to_dom(mode: ThemeMode) {
    apply_data_attrs(mode.scheme(), mode.skin());
}

#[cfg(target_arch = "wasm32")]
fn apply_data_attrs(scheme: &str, skin: &str) {
    let Some(window) = web_sys::window() else { return };
    let Some(doc) = window.document() else { return };
    let Some(html) = doc.document_element() else { return };
    html.set_attribute("data-theme", scheme).ok();
    html.set_attribute("data-skin", skin).ok();
}

#[cfg(not(target_arch = "wasm32"))]
fn apply_data_attrs(_scheme: &str, _skin: &str) {
    // SSR 阶段没 DOM 可写；CSS 在 hydrate 时一次性接管即可。
}
```

删掉旧的 `apply_data_theme`（单属性版）与它的 wasm / non-wasm 两份实现。

`read_storage` 改为「localStorage → DOM 属性 → 默认」三级兜底：

```rust
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
    // localStorage 没有或不可用 → 读内联脚本写好的 DOM 属性。
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
```

`write_storage` 不用改（它调 `mode.as_str()`，四态字符串自动生效）。

- [ ] **Step 2: 重写 `ThemeButton`（CSS 驱动图标，消除 SSR/hydrate 子树失配）**

现状是 `{move || match mode.get() { … }.into_any()}` —— SSR 拿不到 localStorage、hydrate 拿得到，两边渲染**不同的 SVG 子树**。改成两枚图标永远都在 DOM 里、由 CSS 决定显示哪一枚，DOM 结构在 SSR 与 hydrate 之间完全一致。

把 `theme.rs` 里的 `ThemeButton` 整个替换为：

```rust
/// 主题按钮：四态循环 —— 经典亮 → 经典暗 → Material 亮 → Material 暗。
///
/// **两枚图标永远都渲染在 DOM 里**，显示哪一枚由 CSS 按 `<html data-theme>`
/// 决定（见 `style/main.css` 的 `.theme-btn .ti` 规则）。这样做的直接好处是
/// SSR 与 hydrate 渲染出的 DOM 结构完全一致 —— 原先那种
/// `{match mode.get() { … }.into_any()}` 写法在 SSR（读不到 localStorage）
/// 与 hydrate（读得到）之间会渲染出两棵不同的 SVG 子树，是一处既存的
/// hydration 隐患。
///
/// 「经典 vs Material」靠**颜色通道**表达：Material 态下按钮图标染成
/// `--md-primary`（靛蓝），经典态保持中性灰。图标本身始终是太阳/月亮，
/// 只负责表达亮暗。
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
```

`ic_moon` 需要加进 import（`ic_sun` 已经在）：把 `use super::icons::{ic_moon, ic_sun};` 改成 `use super::icons::{ic_moon, ic_sun};`（原本已有两个，确认一下；若原本只 import 了 `ic_moon, ic_sun` 则无需改）。

同时删掉原来 `ThemeMode::System` 分支里的那段「左半暗 + 右半亮」内联 SVG（`System` 态已不存在）。

- [ ] **Step 3: 给 `.theme-btn` 加 CSS 驱动的图标显隐规则**

在 `style/main.css` 的 `.theme-btn` 规则块（L2645 附近）**之后**追加：

```css
/* 主题按钮的两枚图标都渲染在 DOM 里，由 CSS 按 <html data-theme> 决定显示
   哪一枚 —— 这样 SSR 与 hydrate 的 DOM 结构完全一致，不会出现子树失配。
   （原先的 {match mode.get(){…}.into_any()} 写法在 SSR 与 hydrate 之间会
   渲染出两棵不同的 SVG 子树。） */
.theme-btn .ti {
    display: none;
}
.theme-btn .ti > svg {
    display: block;
}
:root[data-theme="light"] .theme-btn .ti-sun {
    display: inline-flex;
}
:root[data-theme="dark"] .theme-btn .ti-moon {
    display: inline-flex;
}
```

- [ ] **Step 4: 扩展 head 内联脚本（首屏防色闪 + 旧值迁移）**

`src/app.rs:143-145` 现在是：

```rust
                <script>
                    "(function(){try{var s=localStorage.getItem('rodeo_theme');if(s==='light'||s==='dark'){document.documentElement.setAttribute('data-theme',s);}}catch(e){}})();"
                </script>
```

替换为：

```rust
                // 主题早应用脚本：必须在 HashedStylesheet 之前 inline，否则
                // CSS 第一次解析时还没有 data-theme / data-skin 属性，会看到
                // 几毫秒的色闪。这段脚本做三件事：
                //   1. 读 localStorage.rodeo_theme（四个具体值之一）；
                //   2. 旧值 "system" 或没有值 → 按 prefers-color-scheme 解析成
                //      具体的经典亮/暗，并固化回写 localStorage（存量用户首屏
                //      颜色与改动前完全一致，只是从此不再跟随 OS 昼夜切换）；
                //   3. 把解析结果写到 <html> 的 data-theme / data-skin 上。
                // 同步执行，跑在 DOM 构建前。注意：这里用的是普通 <script>
                // 元素，不是 leptos_meta 的注册式 <Script>——后者会让 head 的
                // meta 游标与 SSR 输出错位，进而 hydration panic。
                <script>
                    "(function(){try{var s=localStorage.getItem('rodeo_theme');var v;if(s==='light'||s==='dark'||s==='material-light'||s==='material-dark'){v=s;}else{v=(window.matchMedia&&window.matchMedia('(prefers-color-scheme: dark)').matches)?'dark':'light';try{localStorage.setItem('rodeo_theme',v);}catch(e2){}}var h=document.documentElement;h.setAttribute('data-theme',(v==='dark'||v==='material-dark')?'dark':'light');h.setAttribute('data-skin',(v==='material-light'||v==='material-dark')?'material':'classic');}catch(e){}})();"
                </script>
```

- [ ] **Step 5: 更新六种语言的 `theme` 段**

`locales/*.json` 的 `theme` 段从 4 个 key 改成 5 个（删 `title_system` / `title_light` / `title_dark`，加 4 个新的）：

| locale | title_classic_light | title_classic_dark | title_material_light | title_material_dark | aria |
|---|---|---|---|---|---|
| en | Theme: classic light (click to change) | Theme: classic dark (click to change) | Theme: Material light (click to change) | Theme: Material dark (click to change) | Toggle theme |
| zh | 主题：经典亮色（点击切换） | 主题：经典暗色（点击切换） | 主题：Material 亮色（点击切换） | 主题：Material 暗色（点击切换） | 切换主题 |
| ja | テーマ：クラシックライト（クリックで変更） | テーマ：クラシックダーク（クリックで変更） | テーマ：マテリアルライト（クリックで変更） | テーマ：マテリアルダーク（クリックで変更） | テーマを切り替え |
| pt | Tema: clássico claro (clique para alterar) | Tema: clássico escuro (clique para alterar) | Tema: Material claro (clique para alterar) | Tema: Material escuro (clique para alterar) | Alternar tema |
| es | Tema: clásico claro (clique para cambiar) | Tema: clásico oscuro (clique para cambiar) | Tema: Material claro (clique para cambiar) | Tema: Material oscuro (clique para cambiar) | Cambiar tema |
| ar | السمة: كلاسيكي فاتحة (انقر للتغيير) | السمة: كلاسيكي داكنة (انقر للتغيير) | السمة: ميتيريال فاتحة (انقر للتغيير) | السمة: ميتيريال داكنة (انقر للتغيير) | تبديل السمة |

用 python 脚本批量写入（保持 JSON 缩进 2 空格、`ensure_ascii=False`，与现有文件格式一致）：

```bash
cd /Users/wangxiaoyan/Works/git/rodeo && python3 - <<'PY'
import json, collections
T = {
 'en': ("Theme: classic light (click to change)","Theme: classic dark (click to change)","Theme: Material light (click to change)","Theme: Material dark (click to change)","Toggle theme"),
 'zh': ("主题：经典亮色（点击切换）","主题：经典暗色（点击切换）","主题：Material 亮色（点击切换）","主题：Material 暗色（点击切换）","切换主题"),
 'ja': ("テーマ：クラシックライト（クリックで変更）","テーマ：クラシックダーク（クリックで変更）","テーマ：マテリアルライト（クリックで変更）","テーマ：マテリアルダーク（クリックで変更）","テーマを切り替え"),
 'pt': ("Tema: clássico claro (clique para alterar)","Tema: clássico escuro (clique para alterar)","Tema: Material claro (clique para alterar)","Tema: Material escuro (clique para alterar)","Alternar tema"),
 'es': ("Tema: clásico claro (clique para cambiar)","Tema: clásico oscuro (clique para cambiar)","Tema: Material claro (clique para cambiar)","Tema: Material oscuro (clique para cambiar)","Cambiar tema"),
 'ar': ("السمة: كلاسيكي فاتحة (انقر للتغيير)","السمة: كلاسيكي داكنة (انقر للتغيير)","السمة: ميتيريال فاتحة (انقر للتغيير)","السمة: ميتيريال داكنة (انقر للتغيير)","تبديل السمة"),
}
for loc,(cl,cd,ml,md,aria) in T.items():
    p = f'locales/{loc}.json'
    d = json.load(open(p, encoding='utf-8'), object_pairs_hook=collections.OrderedDict)
    d['theme'] = collections.OrderedDict([
        ('title_classic_light', cl), ('title_classic_dark', cd),
        ('title_material_light', ml), ('title_material_dark', md), ('aria', aria),
    ])
    with open(p, 'w', encoding='utf-8') as f:
        json.dump(d, f, ensure_ascii=False, indent=2)
        f.write('\n')
    print('ok', p)
PY
```

跑完后核对没有把别的段落弄乱：

```bash
git diff --stat locales/
```

Expected: 六个文件，每个只改 `theme` 段（diff 里应该只看到 theme 段的前后变化）。

- [ ] **Step 6: 类型检查 + 构建**

```bash
make check
```

Expected: 通过。若 `theme.rs` 有未使用 import（如 `ThemeMode::System` 残留），编译器会指出来。

- [ ] **Step 7: 提交**

```bash
git add src/frontend/theme.rs src/app.rs style/main.css locales/
git diff --cached --stat
git commit -m "feat(theme): 主题改四态循环（经典亮/暗 ↔ Material 亮/暗），去掉跟随系统

状态模型从 System/Light/Dark 三态改成四态，落到 <html> 上两个正交属性：
data-theme（light/dark）与 data-skin（classic/material）。两个属性正交是
整套方案的地基 —— CSS 侧「亮暗」和「设计语言」可以各自独立写规则，不必把
四种组合硬编码进选择器。

迁移：旧值 system 或缺失 → head 内联脚本按 prefers-color-scheme 解析成经典
亮/暗并固化回写 localStorage。存量用户首屏颜色一像素不变，唯一的行为变化
是从此不再跟随 OS 昼夜切换。

顺带消除一处既存的 hydration 隐患：原先 ThemeButton 用
{match mode.get(){…}.into_any()} 渲染图标，SSR 读不到 localStorage 而
hydrate 读得到，两边会渲染出两棵不同的 SVG 子树。现在两枚图标永远都在
DOM 里，由 CSS 按 data-theme 决定显示哪一枚，DOM 结构两侧完全一致。

图标沿用现有的 ic_sun / ic_moon，用 <span class=\"ti\"> 包一层做 CSS 的
作用对象（ic_sun 内部把 class=\"ic\" 写死了，挂不上额外类名）。经典 vs
Material 靠颜色通道区分：Material 态图标染 --md-primary。

head 内联脚本仍是普通 <script> 元素而非 leptos_meta 注册式 <Script> ——
后者会让 head 的 meta 游标与 SSR 输出错位进而 hydration panic。"
```

---

### Task 3: Material 令牌层

**Files:**
- Modify: `style/main.css`（在经典段的 `:root[data-theme="dark"]` 块之后追加新段）

**Interfaces:**
- Consumes: Task 1 的 token 机制（`var(--token, 回退)`）
- Produces: `--md-*` 全套 M3 令牌 + 经典 token 在 Material 态的重新映射。供 Task 4/5/6 引用。

- [ ] **Step 1: 追加 Material 令牌层**

在 `style/main.css` 里，`/* ===== 滚动条 ===== */` 注释**之前**（即经典段结束处）插入以下两块。**顺序是关键**：Material 亮 → Material 暗（@media）→ Material 暗（显式），与经典段「跟随系统 < 显式」同一纪律。

```css
/* ============================================================================
   Material 皮肤 —— 令牌层
   ============================================================================
   这一段只定义「值」，不含任何选择器规则；真正的形状/层次差异在下面的
   「Material 结构覆盖」段。

   两个正交属性：`data-theme`（亮暗）× `data-skin`（设计语言）。
   下面的三块与经典段一一对应，顺序同样是「跟随系统 < 显式」：
     1. :root[data-skin="material"]                                亮
     2. @media dark :root[data-skin="material"]:not([data-theme=light])  暗
     3. :root[data-skin="material"][data-theme="dark"]             暗（显式）

   特异度：块 3 是 (0,3,0)，压得住块 1 的 (0,2,0)；块 2 因为多了
   :not([data-theme="light"]) 也是 (0,3,0)，能压住块 1。 */

:root[data-skin="material"] {
    /* ---- Material 3 原始令牌（亮色） ---- */
    --md-primary: #4F46E5;
    --md-on-primary: #FFFFFF;
    --md-primary-container: #E0E7FF;
    --md-on-primary-container: #1E1B4B;
    --md-secondary: #6D5CE7;
    --md-on-secondary: #FFFFFF;
    --md-secondary-container: #EAE7FF;
    --md-on-secondary-container: #231A4D;
    --md-tertiary: #FF4D8D;
    --md-on-tertiary: #FFFFFF;
    --md-tertiary-container: #FFDCE7;
    --md-on-tertiary-container: #3F0021;
    --md-error: #E5484D;
    --md-on-error: #FFFFFF;
    --md-error-container: #FFE1E1;
    --md-on-error-container: #410002;
    --md-surface: #FCFBFF;
    --md-surface-container-lowest: #FFFFFF;
    --md-surface-container-low: #F4F2FF;
    --md-surface-container: #EDEAFF;
    --md-surface-container-high: #E4E0FF;
    --md-surface-container-highest: #DCD7FF;
    --md-on-surface: #181830;
    --md-on-surface-variant: #4C4A66;
    --md-outline: #7B7A9C;
    --md-outline-variant: #C6C4E0;
    --md-inverse-surface: #2F2F45;
    --md-inverse-on-surface: #F1F0FF;
    /* 阴影染色用的 rgb 三元组（不带括号，配合 rgb(var(--md-shadow) / .14) 用） */
    --md-shadow: 79 70 229;

    /* ---- 语义状态色（绿/黄/红） ----
       经典版把「已完成」绿 chip 直接搬到暗色下会刺眼，所以三档语义色各配
       一套容器色 + 容器上文字色。 */
    --md-pos: #1B8E3C;
    --md-pos-container: #D7F5DC;
    --md-on-pos-container: #0B5227;
    --md-warn: #B35900;
    --md-warn-container: #FFE8C2;
    --md-on-warn-container: #5C3600;
    --md-dgr: #CF222E;
    --md-dgr-container: #FFE1E1;
    --md-on-dgr-container: #410002;

    /* ---- 把经典 token 重新指向 M3 语义 ----
       这是「约 70% 界面自动变 Material」的关键：main.css 里绝大部分颜色
       都是通过 --ink / --bg / --line / --blue 这些 token 取的，这里重新赋值
       就等于一次性换掉全站配色。 */
    --ink: var(--md-on-surface);
    --ink2: var(--md-on-surface-variant);
    --ink3: color-mix(in srgb, var(--md-on-surface-variant) 78%, var(--md-surface));
    --ink4: var(--md-outline);
    --line: var(--md-outline-variant);
    --bg: var(--md-surface);
    --bg2: var(--md-surface-container-low);
    --bg3: var(--md-surface-container);
    --blue: var(--md-primary);
    --pos: var(--md-pos);
    --warn: var(--md-warn);
    --dgr: var(--md-dgr);
    --surface: var(--md-surface-container);
    --border: var(--md-outline-variant);
    --border-soft: var(--md-surface-container);
    --hover: var(--md-surface-container-high);
    --mut: var(--md-on-surface-variant);
    --fg: var(--md-on-surface);
    --toolbar-icon: var(--md-primary);
    --sel-row: color-mix(in srgb, var(--md-primary) 14%, var(--md-surface));
    --sel-row-hover: color-mix(in srgb, var(--md-primary) 22%, var(--md-surface));
    --code-bg: color-mix(in srgb, var(--md-on-surface) 8%, transparent);
    --code-line: color-mix(in srgb, var(--md-on-surface) 14%, transparent);

    /* ---- 形状与层次 ---- */
    --r-s: 12px;
    --r-m: 16px;
    --r-l: 20px;
    --r-pill: 999px;
    --gap-panel: 12px;
    --sh-1: 0 2px 5px rgb(var(--md-shadow) / .14), 0 1px 2px rgb(0 0 0 / .08);
    --sh-2: 0 6px 18px rgb(var(--md-shadow) / .20), 0 2px 6px rgb(0 0 0 / .08);
    --sh-3: 0 12px 32px rgb(var(--md-shadow) / .28), 0 4px 10px rgb(0 0 0 / .10);
    --h-btn: 40px;
    --h-ibtn: 40px;
    --state-hover: rgb(var(--md-on-surface) / .08);
    --state-press: rgb(var(--md-on-surface) / .12);
}

@media (prefers-color-scheme: dark) {
    :root[data-skin="material"]:not([data-theme="light"]) {
        --md-primary: #C3C0FF;
        --md-on-primary: #1E1B8F;
        --md-primary-container: #3331B0;
        --md-on-primary-container: #E3E1FF;
        --md-secondary: #C5C0F0;
        --md-on-secondary: #312B82;
        --md-secondary-container: #3D3893;
        --md-on-secondary-container: #E5E1FF;
        --md-tertiary: #FFB0C8;
        --md-on-tertiary: #640033;
        --md-tertiary-container: #8E0038;
        --md-on-tertiary-container: #FFDCE7;
        --md-error: #FFB4AB;
        --md-on-error: #690005;
        --md-error-container: #93000A;
        --md-on-error-container: #FFDAD6;
        --md-surface: #131320;
        --md-surface-container-lowest: #0E0E1A;
        --md-surface-container-low: #1A1A2B;
        --md-surface-container: #1E1E30;
        --md-surface-container-high: #282839;
        --md-surface-container-highest: #333347;
        --md-on-surface: #E5E1E9;
        --md-on-surface-variant: #C7C4DD;
        --md-outline: #918F9F;
        --md-outline-variant: #47464F;
        --md-inverse-surface: #E5E1E9;
        --md-inverse-on-surface: #2F2F45;
        --md-shadow: 0 0 0;

        --md-pos: #6FDD8B;
        --md-pos-container: #0C3D1E;
        --md-on-pos-container: #C8F5D5;
        --md-warn: #FFB95C;
        --md-warn-container: #4A2C00;
        --md-on-warn-container: #FFE0B8;
        --md-dgr: #FFB4AB;
        --md-dgr-container: #5C0F14;
        --md-on-dgr-container: #FFDAD6;

        --code-bg: color-mix(in srgb, var(--md-on-surface) 14%, transparent);
        --code-line: color-mix(in srgb, var(--md-on-surface) 26%, transparent);
    }
}

:root[data-skin="material"][data-theme="dark"] {
    --md-primary: #C3C0FF;
    --md-on-primary: #1E1B8F;
    --md-primary-container: #3331B0;
    --md-on-primary-container: #E3E1FF;
    --md-secondary: #C5C0F0;
    --md-on-secondary: #312B82;
    --md-secondary-container: #3D3893;
    --md-on-secondary-container: #E5E1FF;
    --md-tertiary: #FFB0C8;
    --md-on-tertiary: #640033;
    --md-tertiary-container: #8E0038;
    --md-on-tertiary-container: #FFDCE7;
    --md-error: #FFB4AB;
    --md-on-error: #690005;
    --md-error-container: #93000A;
    --md-on-error-container: #FFDAD6;
    --md-surface: #131320;
    --md-surface-container-lowest: #0E0E1A;
    --md-surface-container-low: #1A1A2B;
    --md-surface-container: #1E1E30;
    --md-surface-container-high: #282839;
    --md-surface-container-highest: #333347;
    --md-on-surface: #E5E1E9;
    --md-on-surface-variant: #C7C4DD;
    --md-outline: #918F9F;
    --md-outline-variant: #47464F;
    --md-inverse-surface: #E5E1E9;
    --md-inverse-on-surface: #2F2F45;
    --md-shadow: 0 0 0;

    --md-pos: #6FDD8B;
    --md-pos-container: #0C3D1E;
    --md-on-pos-container: #C8F5D5;
    --md-warn: #FFB95C;
    --md-warn-container: #4A2C00;
    --md-on-warn-container: #FFE0B8;
    --md-dgr: #FFB4AB;
    --md-dgr-container: #5C0F14;
    --md-on-dgr-container: #FFDAD6;

    --code-bg: color-mix(in srgb, var(--md-on-surface) 14%, transparent);
    --code-line: color-mix(in srgb, var(--md-on-surface) 26%, transparent);
}
```

- [ ] **Step 2: 校验特异度（四种组合各自命中正确的块）**

用一个最小 HTML 在浏览器里验证（或用 Task 7 的 E2E 脚本断言）。人工核对表：

| data-theme | data-skin | 期望 | 命中块 |
|---|---|---|---|
| light | classic | 经典亮 | A 的 `:root` |
| dark | classic | 经典暗 | A 的 `:root[data-theme=dark]` (0,2,0) |
| light | material | Material 亮 | B1 (0,2,0)，晚于 A |
| dark | material | Material 暗 | B3 (0,3,0)，压过 B1 |

**注意**：B3 必须在 B1 **之后**、B2 必须在 B1 之后但**不需要**在 B3 之后（B2 与 B3 特异度相同，B3 靠源码顺序取胜即可）。上面的写法里 B2 在 B1 前、B3 在 B2 后 —— B2 与 B3 特异度都是 (0,3,0)，B3 在后面，所以 B3 赢。**这是对的**。

- [ ] **Step 3: 提交**

```bash
git add style/main.css
git commit -m "feat(css): Material 3 令牌层（靛蓝+珊瑚，亮/暗两套）

在经典段之后插入 Material 令牌层：完整 M3 令牌（primary/secondary/
tertiary 三组 + 五级 surface-container + outline + error）+ 绿/黄/红三档
语义色的独立暗色变体。

关键手法：把经典 token 名字（--ink/--bg/--line/--blue/--pos/…）重新指向
M3 语义。main.css 里绝大部分颜色都经由这些 token 取值，这里重新赋值等于
一次性换掉全站配色 —— 约 70% 的界面自动获得 Material 观感，后续只需在
结构覆盖段处理形状/层次/状态层的差异。

三块（亮 → @media 暗 → 显式暗）与经典段同一纪律，源码顺序保证
data-theme=data-theme=dark 的显式块 (0,3,0) 压得住亮色块 (0,2,0)。"
```

---

### Task 4: Material 结构覆盖 —— 基础控件

**Files:**
- Modify: `style/main.css`（在 Material 令牌层之后追加「结构覆盖」段的第一部分）

**Interfaces:**
- Consumes: Task 3 的 `--md-*` 令牌与 `--sh-*` / `--r-*` / `--h-*` / `--state-*` token
- Produces: 基础控件的 Material 形态。

- [ ] **Step 1: 追加「结构覆盖」段的开头 + state layer 工具类**

在 `style/main.css` 末尾（`/* ===== 工作空间切换器 ===== */` 段之后）追加：

```css
/* ============================================================================
   Material 皮肤 —— 结构覆盖（基础控件）
   ============================================================================
   统一前缀 :root[data-skin="material"]。这一段处理形状、层次、状态层；
   颜色一律引用令牌层定义的 --md-*，不写死色值。 */

/* ---- M3 state layer ----
   hover / 按下时在元素上叠一层半透明。纯 CSS，无 JS，无 hydration 风险。
   Material 的 ripple 需要点击坐标 + Web Animations，本轮明确不做。 */
:root[data-skin="material"] .md-state,
:root[data-skin="material"] .btn,
:root[data-skin="material"] .ibtn,
:root[data-skin="material"] .chip,
:root[data-skin="material"] .colchip,
:root[data-skin="material"] .wside .it,
:root[data-skin="material"] .tbl tbody tr,
:root[data-skin="material"] .pager button {
    position: relative;
    overflow: hidden;
}
:root[data-skin="material"] .md-state::before,
:root[data-skin="material"] .btn::before,
:root[data-skin="material"] .ibtn::before,
:root[data-skin="material"] .chip::before,
:root[data-skin="material"] .colchip::before,
:root[data-skin="material"] .tbl tbody tr::before {
    content: "";
    position: absolute;
    inset: 0;
    background: currentColor;
    opacity: 0;
    transition: opacity 120ms ease;
    pointer-events: none;
}
:root[data-skin="material"] .md-state:hover::before,
:root[data-skin="material"] .btn:hover::before,
:root[data-skin="material"] .ibtn:hover::before,
:root[data-skin="material"] .chip:hover::before,
:root[data-skin="material"] .colchip:hover::before,
:root[data-skin="material"] .tbl tbody tr:hover::before {
    opacity: .08;
}
:root[data-skin="material"] .md-state:active::before,
:root[data-skin="material"] .btn:active::before,
:root[data-skin="material"] .ibtn:active::before,
:root[data-skin="material"] .chip:active::before,
:root[data-skin="material"] .colchip:active::before {
    opacity: .12;
}

/* ---- 按钮 ----
   .btn  → M3 outlined（透明底 + 描边 + 胶囊）
   .btn.pri → M3 filled（primary 底 + on-primary 字 + 一级阴影） */
:root[data-skin="material"] .btn {
    height: var(--h-btn);
    padding: 0 20px;
    border-radius: var(--r-pill);
    background: transparent;
    border: 1px solid var(--md-outline);
    color: var(--md-primary);
    font-weight: 500;
}
:root[data-skin="material"] .btn.pri {
    background: var(--md-primary);
    border-color: var(--md-primary);
    color: var(--md-on-primary);
    box-shadow: var(--sh-1);
}
:root[data-skin="material"] .btn.ghost {
    border-style: dashed;
    border-color: var(--md-outline-variant);
    color: var(--md-on-surface-variant);
}
:root[data-skin="material"] .btn.danger {
    color: var(--md-error);
    border-color: var(--md-error);
}
:root[data-skin="material"] .btn:disabled {
    opacity: .38;
}

/* ---- 图标按钮 ----
   40px 圆形 + state layer。.theme-btn / .timeline-btn / .agent-btn 与
   .ibtn 同族，一并处理。 */
:root[data-skin="material"] .ibtn,
:root[data-skin="material"] .theme-btn,
:root[data-skin="material"] .timeline-btn,
:root[data-skin="material"] .agent-btn {
    width: var(--h-ibtn);
    height: var(--h-ibtn);
    border-radius: 50%;
    color: var(--md-on-surface-variant);
}
:root[data-skin="material"] .ibtn:hover {
    background: transparent;
}

/* ---- 开关 ----
   经典 .switch 是 11px 圆角（Task 1 Step 3 刻意没动它，回退位对不上），
   Material 下改成胶囊。 */
:root[data-skin="material"] .switch {
    border-radius: var(--r-pill);
}

/* ---- 主题按钮：颜色通道 ----
   经典态保持中性灰，Material 态染靛蓝 —— 用户靠颜色一眼分辨当前设计语言。 */
:root[data-skin="material"] .theme-btn {
    color: var(--md-primary);
}

/* ---- 输入框 ----
   M3 filled text field：容器底色、无边框、16px 圆角、focus 时 2px primary 边。 */
:root[data-skin="material"] .inp,
:root[data-skin="material"] .ws-search {
    border: 1px solid transparent;
    border-radius: var(--r-m);
    background: var(--md-surface-container-highest);
    padding: 10px 16px;
    color: var(--md-on-surface);
}
:root[data-skin="material"] .inp:focus,
:root[data-skin="material"] .ws-search:focus-within {
    outline: none;
    border: 2px solid var(--md-primary);
    padding: 9px 15px;
    background: var(--md-surface);
}
:root[data-skin="material"] .ws-search {
    padding: 0 16px;
    min-height: 44px;
}
:root[data-skin="material"] .ws-search input {
    padding: 10px 0;
}

/* ---- 芯片 ----
   M3 assist chip：12px 圆角、容器色 + 容器上文字色。 */
:root[data-skin="material"] .chip {
    height: 26px;
    padding: 0 12px;
    border-radius: var(--r-m);
    font-weight: 500;
}
:root[data-skin="material"] .chip.sel {
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
}
:root[data-skin="material"] .chip.dim {
    background: var(--md-surface-container-high);
    color: var(--md-on-surface-variant);
}
:root[data-skin="material"] .chip.inh {
    border: 1px dashed var(--md-outline-variant);
}
:root[data-skin="material"] .c-doing {
    background: var(--md-tertiary-container);
    color: var(--md-on-tertiary-container);
}
:root[data-skin="material"] .c-done {
    background: var(--md-pos-container);
    color: var(--md-on-pos-container);
}
:root[data-skin="material"] .c-open,
:root[data-skin="material"] .c-p3,
:root[data-skin="material"] .c-wont {
    background: var(--md-surface-container-high);
    color: var(--md-on-surface-variant);
}
:root[data-skin="material"] .c-p0,
:root[data-skin="material"] .c-p1 {
    background: var(--md-error-container);
    color: var(--md-on-error-container);
}
:root[data-skin="material"] .c-p2 {
    background: var(--md-warn-container);
    color: var(--md-on-warn-container);
}

/* ---- 卡片 / 面板 ----
   M3 card：20px 圆角、无边框（暗色补描边）、一级阴影。 */
:root[data-skin="material"] .panel {
    border: 1px solid transparent;
    border-radius: var(--r-l);
    background: var(--md-surface);
    box-shadow: var(--sh-1);
}
:root[data-skin="material"][data-theme="dark"] .panel {
    border-color: var(--md-outline-variant);
}
```

- [ ] **Step 2: 顶栏**

追加：

```css
/* ---- 顶栏 ----
   M3 top app bar：64px、primary-container 底、无下边框、一级阴影。 */
:root[data-skin="material"] .appbar {
    height: 64px;
    padding: 0 20px;
    background: var(--md-primary-container);
    color: var(--md-on-primary-container);
    border-bottom: 0;
    box-shadow: var(--sh-1);
    gap: 20px;
}
:root[data-skin="material"] .appbar-brand {
    font-size: 22px;
    font-weight: 500;
    color: var(--md-on-primary-container);
}
:root[data-skin="material"] .appbar-nav {
    height: 40px;
    padding: 0 16px;
    border-radius: var(--r-pill);
    color: var(--md-on-primary-container);
}
:root[data-skin="material"] .appbar-nav:hover {
    background: rgb(var(--md-on-primary-container) / .08);
    color: var(--md-on-primary-container);
}
```

- [ ] **Step 3: 提交**

```bash
git add style/main.css
git commit -m "feat(css): Material 3 结构覆盖 —— 顶栏/按钮/输入框/芯片/卡片

统一 :root[data-skin=\"material\"] 前缀，形状与层次一律引用令牌层的
--md-* / --sh-* / --r-* / --h-* / --state-*，不写死色值。

- state layer：hover/press 叠半透明层，纯 CSS（真 ripple 需要点击坐标，
  本轮明确不做）
- .btn → M3 outlined 胶囊；.btn.pri → M3 filled + 一级阴影
- .ibtn / .theme-btn / .timeline-btn / .agent-btn → 40px 圆形
- .inp / .ws-search → M3 filled text field
- .chip 及状态色 → M3 assist chip，语义色用各自的 container 色
- .panel → M3 card：20px 圆角、无边框（暗色补描边）、一级阴影
- .appbar → M3 top app bar：64px、primary-container 底、无下边框
- .theme-btn → 颜色通道：Material 态染 --md-primary，经典态保持中性灰"
```

---

### Task 5: Material 结构覆盖 —— 导航与数据

**Files:**
- Modify: `style/main.css`（追加到结构覆盖段）

**Interfaces:**
- Consumes: Task 3/4 的令牌与基础控件
- Produces: 侧栏、表格、筛选栏、分页器、滚动条、文本选中的 Material 形态。

- [ ] **Step 1: 追加导航与数据相关规则**

```css
/* ---- 左侧视图栏 ----
   M3 navigation drawer item：56px 高、胶囊圆角、选中态 secondary-container。 */
:root[data-skin="material"] .wside .it {
    min-height: 44px;
    padding: 8px 12px;
    border-radius: var(--r-pill);
    color: var(--md-on-surface-variant);
}
:root[data-skin="material"] .wside .it.on {
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
    font-weight: 500;
}
:root[data-skin="material"] .wside .grp,
:root[data-skin="material"] .wside .sb-group {
    color: var(--md-on-surface-variant);
    font-weight: 500;
}
:root[data-skin="material"] .wside .it.base {
    border-bottom-color: var(--md-outline-variant);
}

/* ---- 表格 ----
   M3 data table：表头容器色底、48px 行高、只有横向分隔线、hover 叠 state layer。 */
:root[data-skin="material"] .tbl th {
    background: var(--md-surface-container);
    color: var(--md-on-surface-variant);
    font-weight: 500;
    padding: 12px 16px;
    border-bottom: 1px solid var(--md-outline-variant);
}
:root[data-skin="material"] .tbl td {
    padding: 12px 16px;
    border-bottom: 1px solid var(--md-outline-variant);
    height: 48px;
}
:root[data-skin="material"] .tbl tbody tr {
    background: var(--md-surface);
}
:root[data-skin="material"] .tbl tbody tr:hover {
    background: var(--md-surface-container-low);
}

/* ---- 筛选栏 ---- */
:root[data-skin="material"] .filters {
    background: var(--md-surface-container);
    border-top: 1px solid var(--md-outline-variant);
    padding: 10px 20px;
    gap: 12px;
}

/* ---- 分页器：M3 segmented button ---- */
:root[data-skin="material"] .pager button {
    border-radius: var(--r-pill);
    border: 1px solid var(--md-outline-variant);
    background: transparent;
    color: var(--md-on-surface-variant);
    min-width: 40px;
    height: 40px;
}
:root[data-skin="material"] .pager button.on,
:root[data-skin="material"] .pager button[aria-current="page"] {
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
    border-color: transparent;
}

/* ---- 滚动条 ---- */
:root[data-skin="material"] ::-webkit-scrollbar-thumb {
    background: var(--md-surface-container-highest);
    border-radius: 3px;
}
:root[data-skin="material"] ::-webkit-scrollbar-thumb:hover {
    background: var(--md-outline);
}

/* ---- 文本选中 ---- */
:root[data-skin="material"] ::selection {
    background: color-mix(in srgb, var(--md-primary) 30%, transparent);
}
```

- [ ] **Step 2: 提交**

```bash
git add style/main.css
git commit -m "feat(css): Material 3 结构覆盖 —— 侧栏/表格/筛选栏/分页器/滚动条

- .wside .it → M3 navigation drawer item：44px 高、胶囊圆角、选中态
  secondary-container
- .tbl → M3 data table：表头容器色底、48px 行高、只有横向分隔线、
  hover 叠 state layer
- .filters → 容器色底 + 更大的内边距
- .pager button → M3 segmented button：连体胶囊、选中态 secondary-container
- 滚动条 thumb 改 surface-container-highest；文本选中改 primary 30%"
```

---

### Task 6: Material 结构覆盖 —— 浮层与页面

**Files:**
- Modify: `style/main.css`（追加到结构覆盖段）

**Interfaces:**
- Consumes: Task 3/4/5
- Produces: 弹窗、菜单、Agent 面板、命令面板、登录、时间轴、Entry 页、设置页。

- [ ] **Step 1: 追加浮层规则**

```css
/* ---- 对话框 ----
   M3 dialog：28px 圆角、三级阴影、无边框。 */
:root[data-skin="material"] .dmodal,
:root[data-skin="material"] .dmbox {
    border-radius: 28px;
    border: 0;
    background: var(--md-surface-container-high);
    color: var(--md-on-surface);
    box-shadow: var(--sh-3);
}

/* ---- 菜单 / 下拉 ----
   M3 menu：12px 圆角、二级阴影、菜单项 40px 高。 */
:root[data-skin="material"] .avatar-drop,
:root[data-skin="material"] .vmenu,
:root[data-skin="material"] .emenu,
:root[data-skin="material"] .ws-switch-drop,
:root[data-skin="material"] .lang-drop,
:root[data-skin="material"] .newentry-split,
:root[data-skin="material"] .msg-panel,
:root[data-skin="material"] .lblhint,
:root[data-skin="material"] .timepick,
:root[data-skin="material"] .cpick-pop,
:root[data-skin="material"] .entry-picker-results {
    border-radius: var(--r-m);
    border: 1px solid transparent;
    background: var(--md-surface-container-high);
    color: var(--md-on-surface);
    box-shadow: var(--sh-2);
}
:root[data-skin="material"] .avatar-drop .mi {
    min-height: 40px;
    padding: 8px 12px;
    border-radius: var(--r-s);
    color: var(--md-on-surface);
}
:root[data-skin="material"] .avatar-drop .mi:hover {
    background: rgb(var(--md-on-surface) / .08);
}

/* ---- AI Agent 侧边面板：M3 side sheet ---- */
:root[data-skin="material"] .agent-panel {
    border-radius: 28px 0 0 28px;
    border: 0;
    background: var(--md-surface-container);
    color: var(--md-on-surface);
    box-shadow: var(--sh-3);
}
```

- [ ] **Step 2: 追加页面级规则**

```css
/* ---- 登录页 ---- */
:root[data-skin="material"] .login-card {
    border-radius: var(--r-l);
    border: 0;
    background: var(--md-surface);
    box-shadow: var(--sh-1);
    padding: 32px;
}
:root[data-skin="material"] .login-card .btn.pri {
    width: 100%;
    height: 48px;
}

/* ---- 工作空间列表卡片 ---- */
:root[data-skin="material"] .ws-card {
    border-radius: var(--r-l);
    border: 0;
    background: var(--md-surface);
    box-shadow: var(--sh-1);
    transition: box-shadow 150ms ease, transform 150ms ease;
}
:root[data-skin="material"] .ws-card:hover {
    box-shadow: var(--sh-2);
    transform: translateY(-2px);
}
:root[data-skin="material"] .ws-card h3 {
    font-size: 18px;
    font-weight: 500;
}

/* ---- Entry 全屏页 ---- */
:root[data-skin="material"] .entry-title {
    font-size: 22px;
    font-weight: 500;
    line-height: 1.3;
}
:root[data-skin="material"] .entry-editor,
:root[data-skin="material"] .entry-side,
:root[data-skin="material"] .entry-comments,
:root[data-skin="material"] .entry-top {
    border-radius: var(--r-l);
    border: 1px solid transparent;
    background: var(--md-surface);
    box-shadow: var(--sh-1);
}
:root[data-skin="material"][data-theme="dark"] .entry-editor,
:root[data-skin="material"][data-theme="dark"] .entry-side,
:root[data-skin="material"][data-theme="dark"] .entry-comments,
:root[data-skin="material"][data-theme="dark"] .entry-top {
    border-color: var(--md-outline-variant);
}
:root[data-skin="material"] .dtabs .tab {
    border-radius: var(--r-pill);
    padding: 6px 16px;
}
:root[data-skin="material"] .dtabs .tab.on {
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
}

/* ---- 时间轴 ---- */
:root[data-skin="material"] .tl .tl-node,
:root[data-skin="material"] .tl-dot {
    background: var(--md-primary);
    border-radius: 50%;
}
:root[data-skin="material"] .tl .tl-line {
    background: var(--md-outline-variant);
}

/* ---- 设置页表格行 ---- */
:root[data-skin="material"] .panel .tbl td,
:root[data-skin="material"] .panel .tbl th {
    height: 52px;
}
```

- [ ] **Step 3: 提交**

```bash
git add style/main.css
git commit -m "feat(css): Material 3 结构覆盖 —— 弹窗/菜单/Agent 面板/登录/Entry 页/时间轴

- .dmodal / .dmbox → M3 dialog：28px 圆角、三级阴影、无边框
- 各类下拉/菜单（avatar-drop / vmenu / emenu / ws-switch-drop / lang-drop /
  newentry-split / msg-panel / lblhint / timepick / cpick-pop /
  entry-picker-results）→ M3 menu：12px 圆角、二级阴影、菜单项 40px 高
- .agent-panel → M3 side sheet：28px 圆角、三级阴影
- .login-card → M3 filled card，主按钮撑满 48px
- .ws-card → M3 card + hover 上浮 + 18px 标题
- Entry 页各面板 → M3 card，标题走 22px/500
- 时间轴节点改 primary 实心圆 + 胶囊标签"
```

---

### Task 7: E2E 验证脚本

**Files:**
- Create: `scripts/material-theme-e2e.mjs`

**Interfaces:**
- Consumes: Task 2 的四态状态机与 `<html data-theme/data-skin>`；Task 3-6 的 CSS
- Produces: 可重复运行的验证脚本 `node scripts/material-theme-e2e.mjs`

- [ ] **Step 1: 写 `scripts/material-theme-e2e.mjs`**

```javascript
// Material 主题浏览器验证脚本（playwright-core）。
//
// 用法：
//   1) 构建：LEPTOS_SITE_ROOT=target/site-smoke cargo leptos build
//   2) 起服务：LEPTOS_SITE_ADDR=127.0.0.1:3099 LEPTOS_SITE_ROOT=target/site-smoke \
//              LEPTOS_HASH_FILES=true LEPTOS_OUTPUT_NAME=rodeo \
//              ./target/debug/rodeo /tmp/material-smoke.toml
//   3) 验证：node scripts/material-theme-e2e.mjs
//
// 踩过的坑（都来自 scripts/editor-smoke.mjs 的经验）：
//   - playwright-core 是 CommonJS，只能按绝对路径 default import。
//   - debug wasm 约 37MB，gzip 下 CompressionLayer 会卡死 >60s；用
//     page.route('**/pkg/*.wasm') 从磁盘 fulfill。
//   - localStorage.rodeo_jwt 是 JSON 编码过的字符串（带引号）。
//   - **不要预置 localStorage.rodeo_theme**：SSR 不读它，预置会 hydration
//     报错。本脚本要测「切换后持久化」，所以在切换之后再读，而不是预置。
//   - hydration 报错只走 console.error，只挂 pageerror 的脚本会「全过」
//     而页面其实是死的 —— 必须监听 console。

import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';
import fs from 'node:fs';
import path from 'node:path';

const BASE = process.env.RODEO_BASE || 'http://127.0.0.1:3099';
const REPO = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const SITE_ROOT = process.env.RODEO_SITE_ROOT || 'target/site-smoke';
const SHOTS = path.join(REPO, 'target/material-shots');
const EXECUTABLE = path.join(
  process.env.HOME,
  'Library/Caches/ms-playwright/chromium-1217/chrome-mac-x64',
  'Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
);

let failures = [];
function check(name, ok, detail) {
  if (ok) console.log(`  ok   ${name}`);
  else { console.log(`  FAIL ${name}${detail ? ` — ${detail}` : ''}`); failures.push(name); }
}
function eq(name, actual, expected) {
  check(name, actual === expected, `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

function publishedWasm() {
  const hashFile = path.join(REPO, 'target/debug/hash.txt');
  if (!fs.existsSync(hashFile)) throw new Error('找不到 target/debug/hash.txt —— 先跑 cargo leptos build');
  const hash = fs.readFileSync(hashFile, 'utf8').split('\n')
    .find((l) => l.startsWith('wasm:'))?.split(/[:\s]+/)[1];
  if (!hash) throw new Error('target/debug/hash.txt 里没有 wasm 哈希');
  const p = path.join(REPO, SITE_ROOT, 'pkg', `rodeo.${hash}.wasm`);
  if (!fs.existsSync(p)) throw new Error(`找不到 ${p} —— hash.txt 与 ${SITE_ROOT}/pkg 不同步，重跑 cargo leptos build`);
  return p;
}

async function login() {
  const res = await fetch(`${BASE}/api/graphql`, {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      query: 'mutation($e: String!, $p: String!) { login(email: $e, password: $p) { token } }',
      variables: { e: 'admin@local', p: 'Admin12345' },
    }),
  });
  const json = await res.json();
  const token = json?.data?.login?.token;
  if (!token) throw new Error(`登录失败：${JSON.stringify(json)}`);
  return token;
}

async function gql(query, variables, token) {
  const res = await fetch(`${BASE}/api/graphql`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${token}` },
    body: JSON.stringify({ query, variables }),
  });
  const json = await res.json();
  if (json.errors) throw new Error(`GraphQL 报错：${JSON.stringify(json.errors)}`);
  return json.data;
}

async function seed(token) {
  const ws = await gql('mutation($n: String!) { createWorkspace(name: $n) { id slug } }',
    { n: `material-${Date.now()}` }, token);
  await gql('mutation($id: ID!, $t: String!) { createEntry(workspaceId: $id, title: $t) { code } }',
    { id: ws.createWorkspace.id, t: 'Material 验证条目' }, token);
  return ws.createWorkspace.slug;
}

const consoleErrors = [];

async function run() {
  if (!fs.existsSync(EXECUTABLE)) throw new Error(`找不到 Chromium：${EXECUTABLE}`);
  fs.mkdirSync(SHOTS, { recursive: true });
  const token = await login();
  const slug = await seed(token);

  const browser = await pw.chromium.launch({ executablePath: EXECUTABLE, headless: true });
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
  const wasm = fs.readFileSync(publishedWasm());
  await ctx.route('**/pkg/*.wasm', (r) =>
    r.fulfill({ status: 200, contentType: 'application/wasm', body: wasm }));
  await ctx.addInitScript((t) => { localStorage.setItem('rodeo_jwt', JSON.stringify(t)); }, token);

  const page = await ctx.newPage();
  page.on('console', (m) => {
    if (m.type() === 'error') consoleErrors.push(m.text());
  });
  page.on('pageerror', (e) => consoleErrors.push(`pageerror: ${e.message}`));

  // ---- 1. 四态循环 ----
  await page.goto(`${BASE}/${slug}`, { waitUntil: 'domcontentloaded', timeout: 120000 });
  await page.waitForSelector('.appbar .theme-btn', { timeout: 60000 });
  await page.waitForFunction(() => !!document.querySelector('.appbar .theme-btn'), { timeout: 30000 });

  const expect = [
    { theme: 'light', skin: 'classic', store: 'light' },
    { theme: 'dark', skin: 'classic', store: 'dark' },
    { theme: 'light', skin: 'material', store: 'material-light' },
    { theme: 'dark', skin: 'material', store: 'material-dark' },
  ];

  // 起点未知（取决于机器的 prefers-color-scheme），先读当前值再从那里走一圈。
  const start = await page.evaluate(() => ({
    theme: document.documentElement.getAttribute('data-theme'),
    skin: document.documentElement.getAttribute('data-skin'),
  }));
  console.log(`  起点：${start.theme}/${start.skin}`);

  for (let i = 0; i < 4; i++) {
    await page.click('.appbar .theme-btn');
    await page.waitForTimeout(250);
    const st = await page.evaluate(() => ({
      theme: document.documentElement.getAttribute('data-theme'),
      skin: document.documentElement.getAttribute('data-skin'),
      store: localStorage.getItem('rodeo_theme'),
    }));
    const exp = expect[i];
    eq(`第 ${i + 1} 次点击 → data-theme`, st.theme, exp.theme);
    eq(`第 ${i + 1} 次点击 → data-skin`, st.skin, exp.skin);
    eq(`第 ${i + 1} 次点击 → localStorage`, st.store, exp.store);
  }

  // ---- 2. 图标始终可见（CSS 驱动的那一枚） ----
  const iconVisible = await page.evaluate(() => {
    const btn = document.querySelector('.appbar .theme-btn');
    if (!btn) return null;
    const vis = (sel) => {
      const el = btn.querySelector(sel);
      if (!el) return false;
      const r = el.getBoundingClientRect();
      return r.width > 0 && r.height > 0;
    };
    return { sun: vis('.ti-sun'), moon: vis('.ti-moon') };
  });
  check('主题按钮恰好显示一枚图标', iconVisible &&
    (iconVisible.sun !== iconVisible.moon),
    JSON.stringify(iconVisible));

  // ---- 3. 刷新后状态保持 ----
  await page.reload({ waitUntil: 'domcontentloaded', timeout: 120000 });
  await page.waitForSelector('.appbar .theme-btn', { timeout: 60000 });
  await page.waitForTimeout(500);
  const afterReload = await page.evaluate(() => ({
    theme: document.documentElement.getAttribute('data-theme'),
    skin: document.documentElement.getAttribute('data-skin'),
    store: localStorage.getItem('rodeo_theme'),
  }));
  eq('刷新后 data-theme 保持', afterReload.theme, 'dark');
  eq('刷新后 data-skin 保持', afterReload.skin, 'material');
  eq('刷新后 localStorage 保持', afterReload.store, 'material-dark');

  // ---- 4. 旧值 system 的迁移 ----
  const ctx2 = await browser.newContext({ ignoreHTTPSErrors: true });
  await ctx2.route('**/pkg/*.wasm', (r) =>
    r.fulfill({ status: 200, contentType: 'application/wasm', body: wasm }));
  await ctx2.addInitScript((t) => { localStorage.setItem('rodeo_jwt', JSON.stringify(t)); }, token);
  await ctx2.addInitScript(() => { localStorage.setItem('rodeo_theme', 'system'); });
  const p2 = await ctx2.newPage();
  const errs2 = [];
  p2.on('console', (m) => { if (m.type() === 'error') errs2.push(m.text()); });
  await p2.goto(`${BASE}/${slug}`, { waitUntil: 'domcontentloaded', timeout: 120000 });
  await p2.waitForSelector('.appbar .theme-btn', { timeout: 60000 });
  await p2.waitForTimeout(500);
  const migrated = await p2.evaluate(() => ({
    theme: document.documentElement.getAttribute('data-theme'),
    skin: document.documentElement.getAttribute('data-skin'),
    store: localStorage.getItem('rodeo_theme'),
  }));
  check('旧值 system 被迁移成具体值',
    ['light', 'dark'].includes(migrated.store),
    JSON.stringify(migrated));
  eq('迁移后仍是经典皮肤', migrated.skin, 'classic');
  check('迁移过程无 console.error', errs2.length === 0, errs2.join(' | '));

  // ---- 5. 各界面截图（Material 亮 / Material 暗） ----
  const pages = [
    ['workspaces', `${BASE}/workspaces`],
    ['workspace', `${BASE}/${slug}`],
    ['settings', `${BASE}/${slug}/settings`],
    ['login', `${BASE}/login`],
  ];
  for (const [name, url] of pages) {
    for (const mode of ['material-light', 'material-dark']) {
      await page.evaluate((m) => localStorage.setItem('rodeo_theme', m), mode);
      await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 120000 });
      await page.waitForTimeout(1200);
      const file = path.join(SHOTS, `${name}-${mode}.png`);
      await page.screenshot({ path: file, fullPage: true });
      console.log(`  截图 ${path.relative(REPO, file)}`);
    }
  }

  // ---- 6. 全程无 console.error（hydration 报错检测） ----
  check('主流程无 console.error', consoleErrors.length === 0,
    consoleErrors.slice(0, 5).join(' | '));

  await browser.close();

  console.log('');
  if (failures.length) {
    console.log(`✗ ${failures.length} 项失败：`);
    failures.forEach((f) => console.log(`  - ${f}`));
    process.exit(1);
  }
  console.log('✓ 全部通过');
}

run().catch((e) => { console.error(e); process.exit(1); });
```

- [ ] **Step 2: 构建并起冒烟服务**

```bash
cd /Users/wangxiaoyan/Works/git/rodeo
# 1) 独立 site root 构建，不碰用户 :3000 的 target/site
LEPTOS_SITE_ROOT=target/site-smoke cargo leptos build

# 2) 独立 data_dir 的冒烟配置
cat > /tmp/material-smoke.toml <<'TOML'
[server]
host = "127.0.0.1"
port = 3099

[storage]
data_dir = "/tmp/rodeo-material-smoke"

[storage.doc]
backend = "rocksdb"

[storage.blob]
backend = "local"
TOML

# 3) 起服务（后台，记住 PID）
LEPTOS_SITE_ADDR=127.0.0.1:3099 LEPTOS_SITE_ROOT=target/site-smoke \
  LEPTOS_HASH_FILES=true LEPTOS_OUTPUT_NAME=rodeo \
  ./target/debug/rodeo /tmp/material-smoke.toml > /tmp/material-smoke.log 2>&1 &
echo $! > /tmp/material-smoke.pid
sleep 3
curl -sS -o /dev/null -w "server: %{http_code}\n" http://127.0.0.1:3099/login
```

**先确认 3099 没被占**（memory 里记过「残留的旧 smoke server 会伪装成产品 bug」）：

```bash
lsof -nP -iTCP:3099 -sTCP:LISTEN || echo "3099 空闲"
```

- [ ] **Step 3: 跑验证脚本**

```bash
node scripts/material-theme-e2e.mjs
```

Expected: 全部 `ok`，末尾 `✓ 全部通过`。若出现 `FAIL`，按名字对照 Task 2/3/4 的断言定位。

- [ ] **Step 4: 经典态逐屏比对**

用同一脚本把 `rodeo_theme` 设成 `light` / `dark`，在 `master` 的构建产物上截同一组页面，逐一 diff。快速做法：切到 `master` 分支重新构建到 `target/site-baseline`，用同样的脚本参数截一遍，再对比。

```bash
# 截经典态基线（当前分支）
RODEO_SHOTS=target/material-shots-classic node scripts/material-theme-e2e.mjs
```

（脚本目前把截图写到固定目录 `target/material-shots`；基线对比可以用 `git stash` + 切分支重建的方式做，或先用 `git worktree` 起一个 master 的独立检出。）

**最低要求**：经典态下 `style/main.css` 的 token 取值与改动前一致（Task 1 Step 4 已静态比对过），且 E2E 在经典态下无 console.error、各页面渲染正常。

- [ ] **Step 5: 关掉自己起的服务**

```bash
kill "$(cat /tmp/material-smoke.pid)" 2>/dev/null
rm -f /tmp/material-smoke.pid
rm -rf /tmp/rodeo-material-smoke
lsof -nP -iTCP:3099 -sTCP:LISTEN || echo "3099 已释放"
```

**必须做** —— 残留的 orphan 服务会占着 :3099，下次跑验证脚本会连到旧二进制，读到旧代码，症状看起来像「我的改动没生效」。

- [ ] **Step 6: 提交**

```bash
git add scripts/material-theme-e2e.mjs
git commit -m "test(ui): Material 主题四态切换与持久化的浏览器验证

覆盖 8 组断言：
1. 四态循环顺序（经典亮→经典暗→Material亮→Material暗）
2. 每次切换后 <html> 的 data-theme / data-skin 与 localStorage 同步
3. 主题按钮恰好显示一枚图标（CSS 驱动的那一枚）
4. 刷新后状态保持
5. 旧值 system 的迁移（解析成具体值 + 仍是经典皮肤）
6. Material 亮/暗两套下各界面截图
7. 全程无 console.error（hydration 报错只走 console.error，
   只挂 pageerror 的脚本会「全过」而页面其实是死的）
8. 截图存到 target/material-shots/

沿用 scripts/editor-smoke.mjs 的已知坑：playwright-core 按绝对路径
default import、wasm 从磁盘 fulfill 绕开 CompressionLayer、
localStorage.rodeo_jwt 是 JSON 编码字符串、不预置 rodeo_theme。"
```

---

## Self-Review

**Spec coverage**

| 设计文档章节 | 覆盖任务 |
|---|---|
| §2 范围与非目标 | Global Constraints + 各 Task 的「不做」 |
| §3.1 四态 / 两个正交属性 | Task 2 Step 1、Task 3 Step 1 |
| §3.2 迁移（去掉跟随系统） | Task 2 Step 4（内联脚本）、Task 7 Step 1（第 5 组断言） |
| §3.3 解析时机与职责划分 | Task 2 Step 1（`read_storage` 三级兜底）、Step 4 |
| §3.4 首屏不闪时序 | Task 2 Step 4 |
| §4.1 token 化 | Task 1 全部 |
| §4.2 文件分层 | Task 1 Step 1、Task 3 Step 1 |
| §4.3 特异度推导 | Task 3 Step 2 |
| §4.4 Material 令牌层 | Task 3 Step 1 |
| §4.5 逐界面规则 | Task 4（基础控件 + 顶栏）、Task 5（导航与数据）、Task 6（浮层与页面） |
| §4.6 字体 | Task 3 Step 1（沿用系统字体栈，M3 字号字重由 Task 4/6 的具体规则给出） |
| §4.7 state layer / 不做 ripple | Task 4 Step 1 |
| §5.1 theme.rs | Task 2 Step 1 |
| §5.2 head 内联脚本 | Task 2 Step 4 |
| §5.3 icons.rs | Task 2 Step 2（不改 icons.rs，用 span 包一层） |
| §5.4 按钮渲染方式 | Task 2 Step 2、Step 3 |
| §5.5 i18n | Task 2 Step 5 |
| §6.1 编译门槛 | 各 Task 的 `make check` |
| §6.2 E2E | Task 7 全部 |
| §6.3 回归边界 | Task 1 Step 4 + Task 7 Step 4 |
| §7 提交拆分 | 本计划的 7 个 Task 对应 7 个 commit |
| §8 风险与回滚 | Task 1 Step 4（静态比对）、Task 7（E2E） |

**缺口检查**：设计文档 §4.5 的表格里 `.c-open` / `.c-wont` / `.c-p3` 等状态类在 Task 4 Step 1 有覆盖；`.ws-search` 在 Task 4 Step 1 有覆盖；`.dtabs` 在 Task 6 Step 2 有覆盖。`.switch` 保持经典 11px 未 token 化（Task 1 Step 3 明确说明），Material 覆盖留在 Task 4 —— 补上：Task 4 Step 1 追加 `:root[data-skin="material"] .switch { border-radius: var(--r-pill); }`。

**类型一致性**：`ThemeMode::parse -> Option<ThemeMode>`、`as_str -> &'static str`、`next -> ThemeMode`、`scheme -> &'static str`、`skin -> &'static str`、`is_material -> bool` —— Task 2 Step 1 定义，Task 2 Step 2 与 Task 3/4 的 CSS 属性名 `data-theme` / `data-skin` 与之对应。E2E 脚本读的正是这两个属性名，一致。

**Placeholder 扫描**：无 TBD / TODO / 「类似 Task N」/ 「补充错误处理」类空话。所有 CSS 与 Rust 代码均已给出实际内容。
