# Material UI 皮肤 设计文档

> 日期：2026-11-03
> 分支：`feature/material-ui`
> 状态：设计已与用户确认（配色=靛蓝+珊瑚；四态切换按钮取代原三态；全应用铺开；经典为默认；图标=太阳月亮+颜色通道；只做 state layer 不做 ripple）
> 依据：浏览器视觉对比（`.superpowers/brainstorm/`，会话 `31814-1793633836`）

---

## 1. 背景与目标

Rodeo 目前只有一套 UI：`style/main.css` 定义的一套自有的中性灰/蓝配色 + 圆角 6/10/12px + 1px 描边卡片。用户希望再有一套**年轻活力**的界面，走 **Material Design** 路线，并且能**通过主题按钮随时切换**。

本轮要交付：

1. 一套完整的 Material 3 风格皮肤（配色靛蓝 `#4F46E5` + 珊瑚 `#FF4D8D`），覆盖**全部**界面。
2. 顶栏那颗主题按钮从「三态循环（跟随系统/亮/暗）」改成「**四态循环**」：经典亮 → 经典暗 → Material 亮 → Material 暗。
3. 经典 UI 保持默认且**外观零变化**——除了主题按钮本身（状态机从三态变四态）。

## 2. 范围与非目标

### 本轮范围

- `style/main.css`：token 化 + Material 令牌层 + Material 结构覆盖层。
- `src/frontend/theme.rs`：四态状态机、持久化、DOM 属性应用。
- `src/app.rs`：head 内联脚本扩展（首屏防色闪）。
- `src/frontend/icons.rs`：主题按钮图标。
- `locales/*.json`（6 种语言）：主题文案。

### 非目标（明确不做）

- **不新增组件**（不引入 FAB、不改按钮位置、不加导航抽屉开关）。只改现有元素的样式，避免凭空多出「新建」入口这类重复操作路径。
- **不改任何 Rust 组件的 DOM 结构**，除主题按钮本身（见 §6.3）。
- 不碰后端、不改数据格式、不改 GraphQL schema。
- 不补 Rust 单元测试（项目惯例，见 `feedback-no-backend-unit-tests`）。
- 不引外网字体（应用需能离线运行）。
- 不做真正的 ripple 动画（见 §5.5）。

## 3. 状态模型

### 3.1 四态

`ThemeMode` 从 `System | Light | Dark` 改为四态，落到 `<html>` 上**两个正交属性**：

| 态 | `data-theme` | `data-skin` | localStorage 值 |
|---|---|---|---|
| 经典亮 | `light` | `classic` | `light` |
| 经典暗 | `dark` | `classic` | `dark` |
| Material 亮 | `light` | `material` | `material-light` |
| Material 暗 | `dark` | `material` | `material-dark` |

两个属性正交是整套方案的地基：CSS 里「亮暗」和「设计语言」可以各自独立地写规则，不必把四种组合硬编码进选择器。

循环顺序固定为 **经典亮 → 经典暗 → Material 亮 → Material 暗 → 经典亮**。

### 3.2 迁移：去掉「跟随系统」这一档

四态没有「跟随系统」。存量用户的 `localStorage.rodeo_theme` 可能是 `system`（或 key 不存在）。处理规则：

1. `light` / `dark` → 直接映射到经典亮 / 经典暗。
2. `system` / 缺失 → 按 `prefers-color-scheme` 解析成 `light` 或 `dark`（经典皮肤），**并把解析结果写回落盘**。

结果：**存量用户首屏颜色一像素不变**。唯一的行为变化是——从此该值被固化，不再跟随 OS 昼夜切换。用户随时可以点按钮改。

`System` 变体从 `ThemeMode` 中删除，`ThemeMode::parse` 对 `system` 返回 `None`，交由上面的迁移逻辑处理。

### 3.3 解析时机与职责划分

`prefers-color-scheme` 的读取放在 **head 内联脚本**里（纯 JS，无需新增 web-sys feature），Rust 侧只负责读一个已经确定的具体值：

| 环节 | 位置 | 职责 |
|---|---|---|
| 首屏防色闪 | `src/app.rs` 的 `<head>` 内联 `<script>`（普通元素，非 leptos_meta 注册式） | 读 localStorage → 解析 → 写 `<html data-theme/data-skin>` → 写回 localStorage |
| SSR | `src/app.rs` 的 `<html>` 标签 | 无属性（内联脚本会在 CSS 解析前补上） |
| hydrate | `src/frontend/theme.rs` | 读 localStorage → 四态信号；读不到则回退读 DOM 属性 |

内联脚本必须留在 `ShellInner` 的 `<head>` 里用**普通 `<script>` 元素**——`leptos_meta` 的注册式 `<Script>` 会让 head 的 meta 游标与 SSR 输出错位，进而 hydration panic（见 `leptos-meta-registered-script-hydration-break`）。现有的主题防闪脚本已经在那里，本轮只是**扩展**它，不是新增一个。

### 3.4 首屏不闪的时序

内联脚本在 `<head>` 中、位于 `<HashedStylesheet>` **之前**执行（同步解析期执行，早于 CSS 首次解析）→ `<html>` 上的两个属性在样式表生效前就已就位 → 不存在色闪。同时 `@media (prefers-color-scheme: dark)` 的经典回退块保留，兜住「JS 被禁用」的情况。

## 4. CSS 架构

### 4.1 核心手法：先 token 化

`main.css` 现在有大量写死的圆角、阴影、控件尺寸。Material 的观感差异主要来自这几项（形状、阴影、控件密度），而不是颜色本身。

所以第一步把写死值抽成 token，**经典值作为默认值写进 `var()` 的回退位**：

```css
.btn {
    border-radius: var(--r-s);              /* 已经是 token */
    box-shadow: var(--sh-1, none);          /* 新增：回退 none = 经典无阴影 */
    height: var(--h-btn, auto);             /* 新增 */
}
```

写法上统一为 `var(--token, <经典原值>)`：

- 经典路径下取值与今天完全一致 → **经典外观零变化**。
- Material 路径下只需在令牌层覆写同一批 token，就能一次性改变全站同类元素的形状与层次。

新增 token（默认值 = 经典现状）：

| token | 经典默认 | Material 取值 | 用途 |
|---|---|---|---|
| `--sh-1` | `none` | `0 2px 5px rgb(var(--md-shadow) / .14), 0 1px 2px rgb(0 0 0 / .08)` | 卡片 / 一级浮层 |
| `--sh-2` | `none` | `0 6px 18px rgb(var(--md-shadow) / .20), 0 2px 6px rgb(0 0 0 / .08)` | 菜单 / 弹窗 |
| `--sh-3` | `none` | `0 12px 32px rgb(var(--md-shadow) / .28), 0 4px 10px rgb(0 0 0 / .10)` | 命令面板 / Agent 面板 |
| `--h-btn` | `auto` | `40px` | 按钮高度 |
| `--h-ibtn` | `28px` | `40px` | 图标按钮高度 / 宽度 |
| `--r-pill` | `6px` | `999px` | 胶囊圆角 |
| `--state-hover` | `transparent` | `rgb(var(--md-on-surface) / .08)` | M3 state layer（hover） |
| `--state-press` | `transparent` | `rgb(var(--md-on-surface) / .12)` | M3 state layer（按下） |

### 4.2 文件分层

全部留在 `style/main.css` 单一文件内（cargo-leptos 的 `style-file` 只支持一个文件；且 `hash-files = true` 依赖内容哈希，多一张表就多一份缓存失效风险）：

```
main.css
├─ A. 经典（现状原样，零改动）
│   ├─ :root                                             亮
│   ├─ @media dark  :root:not([data-theme="light"])      暗（跟随系统）
│   └─ :root[data-theme="dark"]                          暗（显式）
├─ B. Material 令牌层
│   ├─ :root[data-skin="material"]                       亮
│   ├─ @media dark  :root[data-skin="material"]:not([data-theme="light"])   暗（跟随系统）
│   └─ :root[data-skin="material"][data-theme="dark"]    暗（显式）
└─ C. Material 结构覆盖（统一 :root[data-skin="material"] 前缀）
```

### 4.3 特异度推导（四种组合逐一验证）

关键：Material 的亮色块 `:root[data-skin="material"]` 特异度 = (0,2,0)，与经典显式暗色块 `:root[data-theme="dark"]` 同级，靠**源码顺序**取胜；Material 的显式暗色块多一个属性选择器 = (0,3,0)，稳压亮色块。

| 组合 | 生效块 | 结果 |
|---|---|---|
| classic + light | A 的 `:root` | 经典亮 ✓ |
| classic + dark | A 的 `:root[data-theme=dark]` (0,2,0)，晚于 `:root` | 经典暗 ✓ |
| material + light | B 的 `:root[data-skin=material]` (0,2,0)，晚于 A | Material 亮 ✓ |
| material + dark | B 的 `:root[data-skin=material][data-theme=dark]` (0,3,0) | Material 暗 ✓ |
| OS 暗 + material + light | B 的 `@media` 块带 `:not([data-theme="light"])` → **不匹配**；显式 dark 块也不匹配 | Material 亮 ✓ |
| OS 暗 + classic + 无属性 | A 的 `@media` 块 | 经典暗 ✓ |

B 段三块的顺序必须是「亮 → @media 暗 → 显式暗」，与 A 段现有纪律一致。

### 4.4 Material 令牌层内容

Material 皮肤**复用现有 token 名字**（`--ink` / `--bg` / `--line` / `--blue` …），只是把它们重新指向 M3 语义。这样约 70% 的界面自动获得 Material 观感，C 段只需处理形状/层次/状态的差异。

新增 M3 原始令牌（`--md-*`），Material 段内定义经典段不定义：

**亮色**

```
--md-primary:              #4F46E5      --md-on-primary:           #FFFFFF
--md-primary-container:    #E0E7FF      --md-on-primary-container: #1E1B4B
--md-secondary:            #6D5CE7      --md-on-secondary:         #FFFFFF
--md-secondary-container:  #EAE7FF      --md-on-secondary-container:#231A4D
--md-tertiary:             #FF4D8D      --md-on-tertiary:          #FFFFFF
--md-tertiary-container:   #FFDCE7      --md-on-tertiary-container:#3F0021
--md-error:                #E5484D      --md-on-error:             #FFFFFF
--md-error-container:      #FFE1E1      --md-on-error-container:   #410002
--md-surface:              #FCFBFF
--md-surface-container-lowest: #FFFFFF
--md-surface-container-low:    #F4F2FF
--md-surface-container:         #EDEAFF
--md-surface-container-high:    #E4E0FF
--md-surface-container-highest: #DCD7FF
--md-on-surface:           #181830      --md-on-surface-variant:   #4C4A66
--md-outline:              #7B7A9C      --md-outline-variant:      #C6C4E0
--md-inverse-surface:      #2F2F45      --md-inverse-on-surface:   #F1F0FF
--md-shadow:               79 70 229    /* rgb 三元组，供阴影染色 */
```

**暗色**（不是把亮色反过来，而是按 M3 tonal palette 重新推导：主色变浅、强调色降饱和、容器层级靠明度区分）

```
--md-primary:              #C3C0FF      --md-on-primary:           #1E1B8F
--md-primary-container:    #3331B0      --md-on-primary-container: #E3E1FF
--md-secondary:            #C5C0F0      --md-on-secondary:         #312B82
--md-secondary-container:  #3D3893      --md-on-secondary-container:#E5E1FF
--md-tertiary:             #FFB0C8      --md-on-tertiary:          #640033
--md-tertiary-container:   #8E0038      --md-on-tertiary-container:#FFDCE7
--md-error:                #FFB4AB      --md-on-error:             #690005
--md-error-container:      #93000A      --md-on-error-container:   #FFDAD6
--md-surface:              #131320
--md-surface-container-lowest: #0E0E1A
--md-surface-container-low:    #1A1A2B
--md-surface-container:         #1E1E30
--md-surface-container-high:    #282839
--md-surface-container-highest: #333347
--md-on-surface:           #E5E1E9      --md-on-surface-variant:   #C7C4DD
--md-outline:              #918F9F      --md-outline-variant:      #47464F
--md-inverse-surface:      #E5E1E9      --md-inverse-on-surface:   #2F2F45
--md-shadow:               0 0 0
```

**经典 token → Material 映射**（Material 段内重新赋值）

| 经典 token | → | Material 亮 | Material 暗 |
|---|---|---|---|
| `--ink` | → | `--md-on-surface` | 同左 |
| `--ink2` | → | `--md-on-surface-variant` | 同左 |
| `--ink3` | → | `color-mix(in srgb, --md-on-surface-variant 78%, --md-surface)` | 同左 |
| `--ink4` | → | `--md-outline` | 同左 |
| `--line` | → | `--md-outline-variant` | 同左 |
| `--bg` | → | `--md-surface` | 同左 |
| `--bg2` | → | `--md-surface-container-low` | 同左 |
| `--bg3` | → | `--md-surface-container` | 同左 |
| `--blue` | → | `--md-primary` | 同左 |
| `--pos` / `--warn` / `--dgr` | → | 语义色（见下） | 语义色（见下） |

**语义状态色**（绿/黄/红）单独给暗色变体——经典版把「已完成」绿 chip 直接搬到暗色下会刺眼（浏览器对比稿里暴露过这个问题）：

| 语义 | 亮（字 / 容器 / 容器上字） | 暗（字 / 容器 / 容器上字） |
|---|---|---|
| 成功 | `#1B8E3C` / `#D7F5DC` / `#0B5227` | `#6FDD8B` / `#0C3D1E` / `#C8F5D5` |
| 警告 | `#B35900` / `#FFE8C2` / `#5C3600` | `#FFB95C` / `#4A2C00` / `#FFE0B8` |
| 危险 | `#CF222E` / `#FFE1E1` / `#410002` | `#FFB4AB` / `#5C0F14` / `#FFDAD6` |

### 4.5 Material 结构覆盖（逐界面规则）

C 段统一前缀 `:root[data-skin="material"]`。

| 界面 | 经典现状 | Material 改造 |
|---|---|---|
| 顶栏 `.appbar` | 50px 白底 + 1px 下边框 | 64px `--md-primary-container` 底，无下边框，`--sh-1` 阴影，品牌字 22px/500 |
| 主题按钮 `.theme-btn` | 中性灰图标 | **颜色通道**：图标染 `--md-primary`（经典态保持中性灰），用户靠颜色一眼分辨「现在是不是 Material」 |
| 品牌 `.appbar-brand` | 15px/600 | 22px/500，`--md-on-primary-container` |
| 「新建 Entry」 `.newentry-btn` | 描边按钮 | M3 filled button：`--md-primary` 底、胶囊圆角、40px 高 |
| 图标按钮 `.ibtn` | 28px 方形 | 40px 圆形，hover 叠 `--state-hover` |
| 次级按钮 `.btn` | 白底 + 1px 描边 | M3 outlined：透明底 + `--md-outline` 描边 + 胶囊圆角 + 40px 高 |
| 主按钮 `.btn.pri` | `--ink` 底 | M3 filled：`--md-primary` 底 + `--md-on-primary` 字 + 胶囊 + `--sh-1` |
| 危险按钮 `.btn.danger` | 红字红框 | M3 outlined error：`--md-error` 描边与字 |
| 输入框 `.inp` | 1px 描边 + 10px 圆角 | M3 filled text field：`--md-surface-container-highest` 底、无边框、16px 圆角、12px 16px 内边距、focus 时 2px `--md-primary` 边 |
| 搜索框 `.ws-search` | 描边圆角 | 同 `.inp` 的 filled 形态 |
| 芯片 `.chip` | 6px 圆角小色块 | M3 assist chip：12px 圆角、26px 高、容器色 + 容器上字色 |
| 卡片 `.panel` | 1px 描边 + 12px 圆角 | M3 card：20px 圆角、无边框（暗色用 `--md-outline-variant` 描边）、`--sh-1` |
| 左侧视图栏 `.wside .it` | 6px 圆角、28px 高 | M3 navigation drawer item：56px 高、胶囊圆角、选中态 `--md-secondary-container` 底 + `--md-on-secondary-container` 字 |
| 表格 `.tbl` | 1px 边框表格 | M3 data table：表头 `--md-surface-container` 底、行高 48px、hover 叠 `--state-hover`、只有横向分隔线（`--md-outline-variant`） |
| 筛选栏 `.filters` | 顶部 1px 线 | `--md-surface-container` 底、上下内边距加大 |
| 分页器 `.pager` | 描边小按钮 | M3 segmented button：连体胶囊、选中态 `--md-secondary-container` |
| 弹窗 `.dmodal` / `.dmbox` | 描边卡片 | M3 dialog：28px 圆角、`--sh-3`、无边框 |
| 下拉菜单 `.avatar-drop` / `.mi` | 6px 圆角 + 阴影 | M3 menu：12px 圆角、`--sh-2`、菜单项 40px 高、hover 叠 state layer |
| AI Agent 面板 | 描边浮层 | M3 side sheet：28px 圆角、`--sh-3`、滑入动画保留 |
| 命令面板 | 描边浮层 | M3 dialog：28px 圆角、`--sh-3` |
| 登录卡片 `.login-card` | 描边卡片 | M3 filled card：20px 圆角、`--sh-1`、filled text field + 40px 胶囊按钮 |
| 时间轴 `.tl` | 左侧时间戳列表 | 节点改 12px 实心圆 + `--md-primary` 边、连线 `--md-outline-variant`、卡片圆角 16px |
| 滚动条 | 5px，`--ink4` | 6px，`--md-surface-container-highest` 圆角 thumb |
| 文本选中 | 浏览器默认 | `--md-primary` 30% |
| 焦点环 | 2px `--blue` 25% | 2px `--md-primary` + 2px 外扩 |

### 4.6 字体

**沿用系统字体栈**（`-apple-system, "PingFang SC", "Microsoft YaHei", sans-serif`），不引外网字体——应用要能离线跑，vendor 一个 Roboto 子集也要多一份 ~100KB 静态资源与缓存失效风险。

但按 M3 type scale 调字号与字重（Material 段内覆写）：

| 角色 | Material | 应用到 |
|---|---|---|
| body-medium | 14px / 1.45 / 400 | 基础正文（与现状一致） |
| title-large | 22px / 28px / 500 | 页面主标题、Entry 标题 |
| title-medium | 16px / 24px / 500 | 卡片标题、栏目标题 |
| label-large | 14px / 20px / 500 | 按钮 |
| label-medium | 12px / 16px / 500 | 芯片、导航项 |
| label-small | 11px / 16px / 500 | 辅助标签、计数 |

### 4.7 交互反馈

M3 的 state layer：hover / 按下时在元素上叠一层半透明。

```css
:root[data-skin="material"] .btn {
    position: relative;
    overflow: hidden;
}
:root[data-skin="material"] .btn::before {
    content: "";
    position: absolute; inset: 0;
    background: currentColor;
    opacity: 0;
    transition: opacity .12s;
    pointer-events: none;
}
:root[data-skin="material"] .btn:hover::before { opacity: .08; }
:root[data-skin="material"] .btn:active::before { opacity: .12; }
```

纯 CSS，零 JS，零 hydration 风险。

**不做真 ripple**：真 ripple 需要在点击点生成扩散圆，必须有 JS 参与（坐标 + Web Animations）。本轮明确不做——M3 本身也把 ripple 定义为可选层；等这套皮肤稳定后再单独评估。

## 5. Rust 改动清单

### 5.1 `src/frontend/theme.rs`

```rust
pub enum ThemeMode {
    ClassicLight,
    ClassicDark,
    MaterialLight,
    MaterialDark,
}

impl ThemeMode {
    pub fn parse(s: &str) -> Option<Self;   // 四个具体值；"system"/未知 → None
    pub fn as_str(&self) -> &'static str;   // "light" | "dark" | "material-light" | "material-dark"
    pub fn next(&self) -> Self;              // 四态循环
    pub fn scheme(&self) -> &'static str;   // "light" | "dark"  → data-theme
    pub fn skin(&self) -> &'static str;     // "classic" | "material" → data-skin
    pub fn is_material(&self) -> bool;
}
```

- `ThemeController::cycle()` → `self.mode.set(self.mode.get_untracked().next())`。
- `apply_to_dom(mode)` → 同时写 `data-theme` 与 `data-skin`。
- `read_storage()` → 读 localStorage；`parse` 失败（缺失 / `system`）则回退读 `<html>` 上的 `data-theme` + `data-skin` 属性（内联脚本已经写好了）；再失败则 `ClassicLight`。
- `write_storage(mode)` → 写 localStorage 的四态字符串。

**不引入新的 web-sys feature**：`prefers-color-scheme` 的读取在 JS 侧完成，Rust 只读已经确定的值。

### 5.2 `src/app.rs` 的 head 内联脚本

扩展现有主题脚本（`src/app.rs:143`），逻辑：

```js
(function(){
  try {
    var s = localStorage.getItem('rodeo_theme');
    var v;
    if (s === 'light' || s === 'dark' || s === 'material-light' || s === 'material-dark') {
      v = s;
    } else {
      // 旧值 "system" 或缺失 → 按系统偏好解析成经典亮/暗，并固化
      v = (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches)
          ? 'dark' : 'light';
      try { localStorage.setItem('rodeo_theme', v); } catch(e2){}
    }
    var h = document.documentElement;
    h.setAttribute('data-theme', (v === 'dark' || v === 'material-dark') ? 'dark' : 'light');
    h.setAttribute('data-skin',  (v === 'material-light' || v === 'material-dark') ? 'material' : 'classic');
  } catch(e){}
})();
```

**位置不变**：仍在 `ShellInner` 的 `<head>` 内、`<HashedStylesheet>` 之前，用普通 `<script>` 元素。

### 5.3 `src/frontend/icons.rs`

主题按钮图标：**太阳 / 月亮**两枚，直接复用现有的 `ic_sun()` / `ic_moon()`，**icons.rs 不需要改**。

（`ic_sun()` / `ic_moon()` 返回的 `<svg>` 硬编码 `class="ic"`，没法直接挂额外的类名来做 CSS 定向显示。解法见 §5.4：用一层 `<span class="ti ti-sun">` 包住，span 才是 CSS 的作用对象。）

原先那段「左半暗 + 右半亮」的 `System` 组合图标（`theme.rs` 内联 SVG）随 `System` 态一起删除。

### 5.4 主题按钮的渲染方式（重要）

现状是 `{move || match mode.get() { … }.into_any()}`——**SSR 与 hydrate 会渲染不同的 SVG 子树**（SSR 拿不到 localStorage，hydrate 拿得到），这是一处既存的 hydration 隐患。

本轮顺手消除：**两枚图标永远都渲染在 DOM 里，靠 CSS 决定显示哪一枚**。

```rust
<button class="theme-btn" title=… aria-label=… on:click=…>
    <span class="ti ti-sun">{ic_sun()}</span>
    <span class="ti ti-moon">{ic_moon()}</span>
</button>
```

```css
.theme-btn .ti { display: none; }
:root[data-theme="light"] .theme-btn .ti-sun  { display: inline-flex; }
:root[data-theme="dark"]  .theme-btn .ti-moon { display: inline-flex; }
```

用 `<span>` 包一层而不是给 `<svg>` 挂类名，是因为 `ic_sun()` / `ic_moon()` 内部写死了 `class="ic"`、不接受额外类名；span 是 CSS 的作用对象，`<svg class="ic">` 的既有样式（16×16、`fill: currentColor`）不受影响。

好处：

- DOM 结构在 SSR 与 hydrate 之间**完全一致** → 结构层面不可能 hydration 失配。
- 切换状态时图标即时跟随 `<html>` 属性变化，不需要等 Rust 侧重渲染。
- `title` / `aria-label` 仍是响应式的（属性级差异，tachys 会打补丁，不触发结构性 panic）。

Material 态的「颜色通道」：

```css
:root[data-skin="material"] .theme-btn { color: var(--md-primary); }
```

对比度核算：Material 亮色下 `#4F46E5` on `#E0E7FF` ≈ 6.5:1；暗色下 `#C3C0FF` on `#3331B0` ≈ 5.5:1。均过 WCAG AA。

### 5.5 i18n

`theme` 段从 4 个 key 改为 5 个（删 3 个旧的 `title_system` / `title_light` / `title_dark`，加 4 个新的）：

| key | en | zh |
|---|---|---|
| `theme.title_classic_light` | Theme: classic light (click to change) | 主题：经典亮色（点击切换） |
| `theme.title_classic_dark` | Theme: classic dark (click to change) | 主题：经典暗色（点击切换） |
| `theme.title_material_light` | Theme: Material light (click to change) | 主题：Material 亮色（点击切换） |
| `theme.title_material_dark` | Theme: Material dark (click to change) | 主题：Material 暗色（点击切换） |
| `theme.aria` | Toggle theme | 切换主题 |

六种语言（en / zh / ja / pt / es / ar）同步更新。

## 6. 验证方案

### 6.1 编译门槛

```bash
make check     # native + wasm32 两侧类型检查
make build     # 完整 debug 构建
```

### 6.2 E2E 浏览器实测（项目硬性要求）

按 `browser-verification-setup` 的 recipe：

1. `LEPTOS_SITE_ROOT=target/site-smoke cargo leptos build`（**必须用独立 site root**，不能抢用户 :3000 的 `target/site`）。
2. 用临时 `data_dir` 起 :3099 实例，`LEPTOS_HASH_FILES=true`、`LEPTOS_OUTPUT_NAME=rodeo` 必设。
3. `playwright-core` 驱动，**必须挂 `page.on('console')` 监听 `console.error`**——hydration 报错只走 console.error，只挂 `pageerror` 的脚本会「全过」而页面其实是死的。
4. wasm 体积大（debug 约 37MB）+ CompressionLayer 会拖死请求，需按 memory 里的配方 `route.fulfill` 从磁盘喂 wasm。

断言清单：

| # | 断言 |
|---|---|
| 1 | 主题按钮点击 4 次，依次得到 经典亮 → 经典暗 → Material 亮 → Material 暗（检查 `<html>` 的 `data-theme` / `data-skin`） |
| 2 | 每次切换后 `localStorage.rodeo_theme` 为对应四态字符串之一 |
| 3 | 刷新后状态保持（localStorage → 属性 → 按钮图标三处一致） |
| 4 | 旧值 `system` 的存量用户：首屏颜色 = 系统偏好解析结果，且 localStorage 被固化为具体值 |
| 5 | **经典态与改动前逐屏一致**（改动前先在 master 上截基线图） |
| 6 | Material 态下各界面渲染正常（顶栏 / 列表 / 视图主页 / Entry 页 / 设置 / 登录） |
| 7 | 全程无 `console.error`（无 hydration 报错） |
| 8 | 主题按钮图标始终可见（CSS 驱动的两枚图标方案生效） |

### 6.3 回归边界

- 经典 UI 应当**一像素不变**——所以 token 化那一步（§4.1）必须验证经典路径取值与改前一致。
- 切到 Material 再切回经典，DOM 与样式应完全复位（属性驱动的纯 CSS 切换，天然可逆）。

## 7. 分支与提交拆分

分支：`feature/material-ui`（从 master 切出）。

| # | commit | 内容 |
|---|---|---|
| 1 | `docs: Material UI 皮肤设计文档` | 本文件 |
| 2 | `refactor(css): 把圆角/阴影/控件尺寸抽成 token，经典外观不变` | §4.1 的 token 化 + 经典路径取值替换 |
| 3 | `feat(theme): 主题改四态循环（经典亮/暗 ↔ Material 亮/暗），去掉跟随系统` | §5.1 + §5.2 + §5.4 + §5.5 |
| 4 | `feat(css): Material 3 令牌层（靛蓝+珊瑚，亮/暗两套）` | §4.4 |
| 5 | `feat(css): Material 3 结构覆盖 —— 顶栏/按钮/输入框/芯片/卡片` | §4.5 前半 |
| 6 | `feat(css): Material 3 结构覆盖 —— 侧栏/表格/弹窗/浮层/时间轴` | §4.5 后半 |
| 7 | `feat(css): Material 3 覆盖其余界面 —— 登录/设置/管理后台/Entry 页` | §4.5 剩余 |

每个 commit 都要过 `make check`；提交前用 `git diff --cached --stat` 确认只含自己的改动（工作树可能混着别的会话的 WIP，见 `feedback-selective-commit-shared-tree`）。

## 8. 风险与回滚

| 风险 | 缓解 |
|---|---|
| 经典 UI 被 token 化改坏 | 回退位写死经典原值；E2E 断言 5 逐屏比对基线截图 |
| hydration 失配 | 图标改 CSS 驱动，DOM 结构恒定；E2E 断言 7 抓 `console.error` |
| Material 段选择器写错导致漏覆盖 | 逐界面走查 §4.5 表格；E2E 断言 6 覆盖全部界面 |
| 暗色下语义色刺眼 | 语义色单配暗色变体（§4.4 表） |
| 样式文件膨胀到难维护 | 分段横幅 + 严格 `data-skin` 前缀；必要时再抽 `style/material.css`（本轮不做） |

**回滚**：删掉 `<html>` 上的 `data-skin` 属性即回到经典；最坏情况直接 revert 分支。经典路径不依赖任何新逻辑。
