# 编辑器能力补齐：就地开 better-table / syntax 与 Tiptap 迁移

日期：2026-10-28
状态：待评审
关联：`2026-10-28-editor-markdown-tables-design.md`（今天的表格设计稿，本文档吸收其 §3.1 / §3.2 / §3.4 / §3.5 / §3.6）

---

## 1. 目标与背景

需求原文：「有没有比 tiny 更好的开源编辑器，支持插入图片、表格、语法高亮代码块等」。

当前编辑器是仓库内 vendored 的 `@opentiny/fluent-editor` 3.25.12（OpenTiny 基于 Quill 2.0 的分支，MIT，460KB mjs + 171KB css），Rust 侧只通过 `public/tiny-editor/glue.js` 暴露的 `window.__rodeo_tiny_editor__` 桥接。

**但本设计不假定「必须换编辑器」。** 侦察后的事实是：用户点名的三个能力里，**表格和语法高亮当前依赖已经自带、只是没启用**；真正换不动的是「图片」——它现在能用，只是自研的粘贴上传。因此本设计分两段：

| 段 | 内容 | 决策闸口 |
|---|---|---|
| **阶段 0** | 在现有 fluent-editor 上把 `better-table` + `modules/syntax` 打开，补齐表格与语法高亮代码块 | 阶段 0 实测通过 → **取消迁移**，本文档的阶段 1 全部作废 |
| **阶段 1** | 迁移到 Tiptap v3（MIT），顺带把正文存储从 Quill Delta 改成 HTML | 只在阶段 0 实测暴露结构性缺陷时才做 |

这样安排的理由：阶段 0 一天内就能交付用户点名的两个能力，且完全可回滚；阶段 1 是一次两周量级的重写，先拿到阶段 0 的实测结论再决定要不要付这个成本，比现在拍脑袋强。

---

## 2. 现状核实（已读 vendored bundle 确认，非推测）

### 2.1 表格：`better-table` 模块已在包里，且**今天就已经在跑**

`public/tiny-editor/fluent-editor.mjs` 顶层 `register({...})` 里含 `"modules/better-table":pr`（bundle 内共 45 处 `better-table` 引用）。

- 主题 `themes/snow` 的 `DEFAULTS.modules` 第一层 key 含 `"better-table": {operationMenu:{color:true}}` —— `resolveOptions` 会把主题默认模块深合并进最终模块表。
- 推论：正文编辑器、评论编辑器、`create()` 的 readonly 分支**今天就已经实例化了 better-table 模块**（它已在 `quill.root` 上挂 mousedown / compositionstart / compositionend / keypress / contextmenu，已经顶掉 `h1..h6` 与 `ol,ul` 的 clipboard matcher，已经重排 Backspace 绑定）。
- 推论：**不要写 `'better-table': true`** —— 那会用 `{}` 覆盖掉主题的 `{operationMenu:{color:true}}`，白白丢掉右键「背景色」菜单。
- 推论：只有 `toHtml` 的共享渲染实例（用 default theme）需要补 `'better-table': true` —— `table-view` blot 的 scroll 处理器会 `getModule("better-table")`，缺模块就 TypeError。
- 主题里已有 `better-table` 的工具栏 handler（`themes/snow` 的 `DEFAULTS.modules.toolbar.handlers["better-table"]` → `insertTable(3,3)`）。Quill 的 `resolveOptions` 对 `modules` 做深合并，数组形式的工具栏会被包成 `{container:[...]}` 再与主题的 `{handlers:{...}}` 深合并 —— handler 不会丢，**工具栏不需要从数组改成对象**。

**结论：表格功能只差工具栏一行 `['better-table']`。** 按钮 class 由 Quill 的 toolbar 模块按格式名生成（`ql-better-table`），图标来自 `ui/icons`（已含 `"better-table"`）。

### 2.2 语法高亮：`modules/syntax` 已注册，缺 highlight.js

同一个 `register({...})` 里含 `"modules/syntax":Ji`。抽出其实现（`Hi` 类）：

- `constructor`：`if (this.options.hljs == null) throw new Error("Syntax module requires highlight.js. Please include the library on the page before Quill.")` —— **highlight.js 必须先在页面上存在**。
- `Hi.DEFAULTS = { hljs: window.hljs, interval: 1000, languages: [ {key:'plain',label:'Plain'}, {key:'bash',...}, {key:'cpp',...}, {key:'cs',...}, {key:'css',...}, {key:'diff',...}, {key:'xml',...}, ... ] }` —— 默认语言表由 Quill 提供。
- `initListener()`：监听 `SCROLL_BLOT_MOUNT`，命中 code-block blot 时**创建一个 `<select>`**，选项就是 `options.languages`，`change` 时执行 `blot.format(bt.blotName, value)` 并重新高亮；随后 `blot.attachUI(select)` —— 语言下拉挂在代码块自己的 `uiNode` 上。
- `highlightBlot(blot, lang)`：`lang === 'plain'` 时按纯文本插；否则 `hljs.highlight(code, {language: lang})`（hljs ≥ 11 的新 API，bundle 里有版本判断分支）→ 写进 `div.ql-code-block` 的 innerHTML → 由 `code-token` blot（`Ae`）承载每个高亮 span。
- `initTimer()`：监听 `SCROLL_OPTIMIZE`，**debounce 1000ms** 后重新高亮所有代码块。

**结论：加一个 `modules: { syntax: { hljs: window.hljs } }` + 页面里放 highlight.js，工具栏的 `code-block` 就会变成带语言下拉的高亮代码块。** 当前工具栏已有 `['blockquote', 'code-block']`，`code-block` 按钮已经在，只是没挂 syntax 模块，所以是纯 `<pre>`。

vendor 源：`highlight.js` npm 包 ≥ 11（bundle 要求 hljs ≥ 11 走新 API）。用 `lib/common` 构建即可覆盖 Quill 默认语言表里的全部语言（bash/cpp/csharp/css/diff/xml/java/javascript/markdown/php/python/ruby/sql 都在 common 集内）。

### 2.3 图片：已可用，但是自研

`glue.js` 的 `attachImageUpload` 在容器上挂 capture 阶段的监听，拦 `clipboardData` 里的图片文件 → 调 Rust 侧 `upload_attachment` GraphQL → `editor.insertEmbed(at, 'image', url, 'user')`。图片是正文的一部分，不列进附件列表。

**三个能力里只有图片需要新做，而它已经是能用的。**

### 2.4 当前 editor 层的全部痛点（与本次迁移相关）

| 痛点 | 位置 | 性质 |
|---|---|---|
| fluent-editor 把列表 blot 的 Delta 形状改成 `{"list":{"value":"ordered"}}`，与 Quill 标准不符 | `glue.js:31-95` 的兼容层 | 自造 |
| 同一份代码有「导出窗口内 patch `ListBlot.formats()`」的第二套 | `glue.js:35-42` | 自造 |
| Safari 26.2 + 拼音在列表项里删掉整个 LI（`<span class="ql-ui">` 是元凶） | `glue.js:59-81` 干掉 `attachUI` | 打的补丁 |
| 工具栏/浮层孤儿节点清扫 | `glue.js:96-120` | 自造 |
| `style/main.css:139-196+` 一整套 `.ql-*` 亮色/暗色覆写 | 因为 fluent-editor 工具栏写死亮色 | 适配 |
| `service/mention.rs::flat_text` 在 Rust 里解析 Delta | `mention.rs:9` | 自造 |
| `service/search/helpers.rs::strip_rich_text` 在 Rust 里解析 Delta | `search/helpers.rs:10` | 自造 |
| `service/comment.rs::has_image_embed` 查 Delta 的 embed | `comment.rs:273` | 自造 |
| AI agent 工具要求 LLM 输出「Quill Delta JSON 字符串」 | `agent/tools/add_comment.rs` | 隐患 |

---

## 3. 阶段 0：在现有编辑器上补齐表格与语法高亮

### 3.1 工具栏加两个按钮

`glue.js` 的 `modules.toolbar` 数组改为：

```js
const modules = {
  toolbar: [
    ['bold', 'italic', 'underline', 'strike'],
    [{ header: 1 }, { header: 2 }, { header: 3 }],
    [{ list: 'ordered' }, { list: 'bullet' }],
    ['blockquote', 'code-block'],
    ['link'],
    ['better-table'],      // ← 新增（模块本身已在跑，见 §2.1）
    ['clean'],
  ],
};
```

- **不要**写 `'better-table': true`（会覆盖主题的右键菜单配置）。
- `code-block` 按钮保持不变，但要在 `modules` 里加 `syntax`：

```js
const modules = {
  syntax: { hljs: window.hljs },   // ← 新增，见 §3.2
  toolbar: [ /* 同上 */ ],
};
```

### 3.2 highlight.js 落地

**vendor 源**：`https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.12.0/highlight.min.js`（129,254 字节，BSD-3-Clause，UMD 风格 —— 顶层 `var hljs = function(){...}`，在浏览器里落到 `window.hljs`）。落到 `public/tiny-editor/hljs.min.js`，跟 `fluent-editor.mjs` 一样 vendor 进仓库，运行时零外网依赖。

**已实测**：该文件在 node 里跑起来是 hljs `11.12.0`，`listLanguages()` 返回 36 种语言 —— `bash c cpp csharp css diff go graphql ini java javascript json kotlin less lua makefile markdown objectivec perl php php-template plaintext python python-repl r ruby rust scss shell sql swift typescript vbnet wasm xml yaml`。**Quill 默认语言表（plain/bash/cpp/cs/css/diff/xml/java/javascript/markdown/php/python/ruby/sql）全部覆盖**。

**加载方式**：`<script src="/tiny-editor/hljs.min.js">` 放在 **shell 的 `<head>`** 里，
用**普通 script 元素**，**不要用 leptos_meta 的注册式 `<Script>` 组件**（详见 §3.2.1）。

**加载顺序**（这是本任务唯一的坑）：

- `fluent-editor.mjs` 在**模块求值时**就执行 `Ji.DEFAULTS = { hljs: window.hljs, ... }` —— `window.hljs` 必须在那之前存在，否则 `DEFAULTS.hljs` 是 `undefined`，模块构造函数会抛 `Syntax module requires highlight.js`。
- `app.rs` 里 `<Script src="/tiny-editor/glue.js" type_="module">` 是 ES module，HTML 规范规定 module script 延迟到文档解析完才执行。经典（非 module、非 async、非 defer）script 在解析到它时立即同步执行。
- 所以：把 hljs 放在**普通 script**里、且排在 glue.js **之前**，就一定早于 `fluent-editor.mjs` 求值。Leptos 的 `<Script>` 组件的 `async_` / `defer` 两个 prop 默认都是 `None`（不渲染该属性），`<Script src="..."/>` 就是阻塞式经典 script。
- 落地写法：`app.rs` 里

  ```rust
  <Stylesheet id="tiny-editor" href="/tiny-editor/style.css"/>
  <Script src="/tiny-editor/hljs.min.js"/>
  <Script type_="module" src="/tiny-editor/glue.js"/>
  ```

- `glue.js` 侧显式传参，不依赖 `DEFAULTS` 的快照：

  ```js
  const modules = {
    syntax: { hljs: window.hljs },
    toolbar: [ /* 见 §3.1 */ ],
  };
  ```

  显式传参的好处：即使将来调整 script 加载顺序导致 `DEFAULTS.hljs` 变 `undefined`，构造函数的 `options.hljs == null` 判定也会因为我们传了值而不抛。

- **落地验证点**：`view-source` 确认 `<script src="/tiny-editor/hljs.min.js">` 在 `<script type="module" src="/tiny-editor/glue.js">` 之前，且浏览器控制台没有 `Syntax module requires highlight.js`。

### 3.2.1 页面级脚本不要用 leptos_meta 的注册式 `<Script>`

实测踩到：**在 App 视图里多加一个 leptos_meta 的注册式 `<Script>`，会让整个前端的
hydration 崩掉。**

leptos_meta 的注册式组件（`Stylesheet` / `Script` / `Title`）把自己收进
`ServerMetaContext`，SSR 时统一渲染到 head 的 `<!--HEAD-->` 标记之后；客户端 hydrate
时，`MetaContext` 的**一个共享游标**按注册顺序逐个消费 head 里的节点
（`leptos_meta-0.8.6/src/lib.rs` 的 `RegisteredMetaTag::hydrate`）。在 App 视图里
多加一个注册式 `<Script>`，游标就跟 SSR 输出对不齐。

症状很隐蔽 —— **SSR 出来的页面看着完全正常，但前端不接管任何交互**：

```
A hydration error occurred while trying to hydrate an element defined at
  leptos_meta-0.8.6/src/stylesheet.rs:38:14
The framework expected a marker node, but found this instead: <header class="appbar">
panicked at tachys-0.2.18/src/hydration.rs:216:9: Unrecoverable hydration error
```

**`page.on('pageerror')` 抓不到它** —— 走的是 `console.error`。只挂 pageerror 的
验证脚本会「全过」而页面实际是死的。

**修法**：页面级脚本依赖不放 App 视图，改放 **shell 的 `<head>`** 里用普通
`<script src=...>` 元素。shell 不参与 hydrate，普通元素就没有这套对齐问题。

### 3.3 markdown 表格语法与粘贴转换

完整规则见 `2026-10-28-editor-markdown-tables-design.md` §3.2 / §3.3 / §3.4 / §3.5，本文不重复。阶段 0 直接执行那份设计稿的 §3.1–§3.5 与 §3.6（AI 对话面板的 markdown 表格）。

**一处修正**：那份设计稿 §3.4 的粘贴拦截要求「光标不在表格里」，判定用 `editor.getFormat(sel.index)` 里没有 `table-cell-line` / `table-col`。开启 syntax 模块后 `code-block` 的格式名不变，此判定不受影响。

### 3.4 阶段 0 的已知风险

**先定一件事：syntax 模块把高亮结果写回模型，我们接受这个形态。**

`highlightBlot()` 返回的是**带 `code-token` 属性的新 Delta**（形如 `{"insert":"fn","attributes":{"code-token":"keyword"}}`）并由调用方灌回 code-block blot。后果是：

1. 打开含代码块的条目 → 模型变 → `text-change` → Rust 侧 `on_change` → 页面变「未保存」态。
2. 落库的 `code-block` 取值是**语言字符串**（`"plain"` / `"rust"`），不是 `true`。
3. 落库的文本带 `code-token` 属性（高亮 token）。

**「抑制」的做法**（在 glue.js 里绕开模块的回写路径）会让代码块失去语言与高亮，得不偿失；**「接受」**的代价是阶段 1 的 Delta→HTML 转换清单必须加上 `code-token`，并且只读渲染路径要能吃下它（阶段 0 的 `toHtml` 实例挂 syntax 就是为此）。所以选「接受」，并把它写进阶段 1 的转换清单与验收项。

| 风险 | 说明 | 应对 |
|---|---|---|
| syntax 模块的 `highlight()` 会**改模型**（把高亮后的 delta 灌回 code-block blot） | 打开含代码块的条目就会触发 `text-change` → Rust 侧 `on_change` → 脏状态 | **接受**（见上）。实测：打开含代码块的条目后，页面是否立刻显示「未保存」；若会，需要在 Rust 侧比对内容是否真的变了再置脏 |
| `initListener` 把 `<select>` 作为 code-block 的 `uiNode` 插进内容 | 与 Safari IME 那个 `<span class="ql-ui">` 是同类结构 | 回归：Safari 拼音在**代码块**内输入（列表项已有修复，代码块是新路径） |
| `highlight()` 里 `this.quill.update(USER)` | 可能影响 `quill.selection.composing` 状态 | 回归：中文输入法在代码块内连续输入 |
| better-table 与 syntax 模块同时在跑，两者都注册 code-block / list 相关 blot | 可能抢注册 | 实测：列表、代码块、表格三者互不干扰 |
| 只读视图里出现语言下拉（`<select>`） | syntax 模块把 select 作为 uiNode 挂进内容 | readonly 分支给容器加 `.ql-readonly` 类，`style/main.css` 里 `.ql-readonly select { display: none }` 藏掉它；`toHtml` 的导出结果再正则剥一次 `<select>` |

### 3.5 阶段 0 的决策闸口

阶段 0 实测完成后，按下面的判据决定是否进入阶段 1：

**取消迁移（继续用 fluent-editor）**，当以下全部成立：

- 表格：能插、能改行列、能合并单元格、能存能取（刷新后还原）、只读视图渲染正确。
- 代码块：语言下拉可用、高亮生效、高亮后的内容能存能取。
- 中文输入法在列表 / 表格单元格 / 代码块里都不丢字。
- 现有的图片上传、@提及、勾选列表无回归。

**进入阶段 1**，当出现下列任一情况：

- 表格缺列宽拖拽 / 单元格内换行等结构性能力，且短期补不上。
- 代码块高亮在 Safari 上有渲染或 IME 问题。
- `glue.js` 的补丁层继续增长，维护成本失控。
- 需要编辑器级的可扩展性（协同编辑、slash 命令、自定义节点）。

---

## 4. 阶段 1：Tiptap v3 迁移（仅在闸口判定「进入」时执行）

### 4.1 选型依据

Tiptap v3（`@tiptap/core` 3.32.0，MIT）底座是 ProseMirror：

- 表格：`@tiptap/extension-table` 的 `TableKit` —— 含 `mergeCells` / `splitCell` / `setCellAttribute` / 可调列宽。
- 代码块高亮：`@tiptap/extension-code-block-lowlight` + `lowlight`（lowlight v3 从 `lowlight/lib/common` 引入语言）。
- 图片：`@tiptap/extension-image`。
- @提及：`@tiptap/extension-mention`（基于 `@tiptap/suggestion`）。
- 勾选列表：`@tiptap/extension-task-list` + `@tiptap/extension-task-item`。
- 其余：StarterKit / Link / Placeholder / Underline 均 MIT。

**全部 MIT 且免费**（Tiptap 2025-06-06 把 Details / Emoji / DragHandle / FileHandler / Mathematics / TableOfContents / UniqueID 等原 Pro 扩展开源为 MIT；付费的只有云文档、AI、转换、审阅等托管服务）。

**排除项**：

- CKEditor 5 —— GPL-2.0+，v44 起还要 `licenseKey: 'GPL'`。Rodeo 走开放核心 + 私有化永久授权的商业路线，copyleft 对闭源商业版不可接受。
- Lexical / Plate / BlockNote —— 渲染层是 React 专属，Leptos 接不上。
- Toast UI / wangeditor / Vditor —— 经典全局实例模型，与 Quill 同病。

**Vanilla JS 用法**：官方支持无框架用法，`new Editor({ element, extensions, content })`，事件用 `editor.on('update', ...)` / 构造参数里的 `onUpdate` 回调。与现有 `window.__rodeo_xxx__` 全局桥接同构。

### 4.2 打包与离线部署

Rodeo 有私有化部署（离线环境），**运行时不得依赖 CDN**。

- 新增仓库根 `package.json`，devDependencies 固定版本：`esbuild`、`@tiptap/core@3.32.0`、`@tiptap/pm@3.32.0`、`@tiptap/starter-kit@3.32.0`、`@tiptap/extension-table@3.32.0`、`@tiptap/extension-image@3.32.0`、`@tiptap/extension-code-block-lowlight@3.32.0`、`@tiptap/extension-mention@3.32.0`、`@tiptap/extension-task-list@3.32.0`、`@tiptap/extension-task-item@3.32.0`、`@tiptap/extension-link@3.32.0`、`@tiptap/extension-placeholder@3.32.0`、`lowlight`。
- `scripts/build-editor.mjs` 用 esbuild 打成单个 ESM：`public/tiptap/editor.mjs`（minify）。
- 产物 **vendor 进仓库**（与现在 `public/tiny-editor/` 的做法一致），运行时零外网依赖；升级 Tiptap 时跑 `make editor` 重新生成。
- `Makefile` 新增 `editor` 目标；`make build` / `make build-release` 依赖它。
- CSS：Tiptap 无内置主题，自写 `public/tiptap/style.css`（ProseMirror 基础排版 + 表格 + 代码块 + hljs token 配色，适配亮/暗两套）。

### 4.3 内容格式：Quill Delta → HTML

**决定：正文在数据库里存消毒后的 HTML 字符串。**

理由：

1. `domain::Entry::detail` 与 `domain::Comment::body` 本来就是 `String`，bincode 结构**不用变** —— 变的只是字符串内容，存量记录的解码不受影响。
2. 只读渲染路径 `comment_list.rs:143` / `share.rs:62` 现在走 `delta_to_html`（**wasm-only**），所以**评论正文在 SSR 里是空的，hydrate 后才出现**。存 HTML 后服务端可以直接渲染，顺手修掉这个洞。
3. `service/mention.rs::flat_text`、`service/search/helpers.rs::strip_rich_text`、`service/comment.rs::is_blank_body` 这三处「在 Rust 里解析 Delta」的代码全部退休成「剥 HTML 标签」。
4. Tiptap 原生吃 HTML（`editor.commands.setContent(html)`），不需要 ProseMirror JSON 的服务端渲染器。

代价：结构化语义（@提及 chip、勾选状态）退化成 HTML 标签。**这可以通过 `data-*` 属性保留**：

| 语义 | HTML 形态 |
|---|---|
| @提及 | `<span data-type="mention" data-id="01H...">@名字</span>` |
| 任务项状态 | `<li data-checked="true">` |
| 代码块语言 | `<pre><code class="language-rust">` |

服务端 `flat_text` 剥标签后仍能得到 `@名字` 纯文本，`extract_mention_names` 的行为不变。

### 4.4 服务端消毒（关键安全项）

**这是本次迁移里唯一一个「不做好就出事」的点。** 现在 HTML 由 Quill 的 `getSemanticHTML` 在**读路径**上消毒（`javascript:` 链接会被洗掉）。改成存 HTML 后，消毒必须移到**写路径**，否则用户能把 `<script>` 存进库，评论列表的 `inner_html` 直接执行。

- 选库：`ammonia` v4.2.4（crates.io 可用，已验证 `cargo add --dry-run` 通过）。
- 消毒点：`service/entry.rs` 的 `create_blank_impl` / `update`，`service/comment.rs` 的 `create` / `update` —— 在写库前对 `detail` / `body` 消毒。
- 白名单必须放行：
  - 标签：`p br strong em u s code pre blockquote h1 h2 h3 h4 h5 h6 ul ol li a img span div hr table thead tbody tfoot tr th td`
  - 属性：`a[href rel target]`, `img[src alt title width height]`, `td/th[colspan rowspan align]`, `code[class]`, `span[data-*]`, `li[data-checked]`
  - `data-*` 用 ammonia 的 `generic_attribute_prefixes`（**具体 API 名称以落地时 crate 文档为准**）。
- 白名单必须禁止：`style`、`on*` 事件属性、`javascript:` / `data:` URL、`<script>` / `<style>` / `<iframe>`。
- **前后端标签契约**：sanitizer 白名单 ⊆ Tiptap schema 认识的标签 ⊇ Tiptap 输出的标签。任一方向漏了，内容就会被静默吃掉。这张表要写进代码注释并在阶段 1 的验证清单里逐项核对。

### 4.5 Rust 侧改造面（Delta 依赖共 7 处）

| # | 位置 | 现状 | 改法 |
|---|---|---|---|
| 1 | `service/mention.rs:9` `flat_text` | 解析 Delta ops 取文本 | 剥 HTML 标签；`<span data-type="mention">` 的文本保留 |
| 2 | `service/mention.rs:69` `preview` | 基于 `flat_text` | 跟随 #1 |
| 3 | `service/search/helpers.rs:10` `strip_rich_text` | 解析 Delta ops 取文本 | 剥 HTML 标签（检索与 AI 摘要共用） |
| 4 | `service/comment.rs:262` `is_blank_body` | Delta→纯文本判空 | 剥标签后判空 |
| 5 | `service/comment.rs:273` `has_image_embed` | 查 Delta 里的 image embed | 查 `<img` 标签 |
| 6 | `service/agent/tools/add_comment.rs` | 工具描述要求 LLM 输出 Delta JSON | **改成要求纯文本**，服务端 escape 后包 `<p>`（LLM 写 Delta JSON 本来就不靠谱） |
| 6b | `service/agent/tools/update_entry.rs:92` | `detail` 参数描述同样要求「Quill Delta JSON 字符串」 | 同 6，一起改成纯文本 |
| 6c | `api/graphql.rs:2615` `summarizeEntries` | 把 AI 生成的 **Markdown 纯文本** summary 直接 `create_with_detail(..., &summary)` 写进 `entry.detail`（既不是 Delta 也不是 HTML） | 服务端统一：正文一律是 HTML，写入前走 `sanitize_html`；Markdown 文本按行包 `<p>` |
| 7 | `src/frontend/tiny_editor.rs:117` `normalize_delta` | Delta 归一化 | 改成 HTML 归一化：空 → 空串；纯文本 → escape + `<p>` 包裹 |

**「正文生产者」一共 4 条**，缺一条就会留下格式不一致的脏数据：前端编辑器、AI agent 的两个工具、AI 摘要。写入点统一收口到一个函数（`richtext::normalize_for_storage`），四条路径都走它。

前端读路径相应改：`comment_list.rs:143` 与 `share.rs:62` 的 `delta_to_html(&x)` → 直接用存储的 HTML。

#### 4.5.1 HTML → 纯文本的抽取规则

这一条规则同时决定 `mention::flat_text`、`search::strip_rich_text`、`comment::is_blank_body`、AI 摘要输入四个行为，必须写死：

| 输入 | 输出 |
|---|---|
| `<p>你好</p><p>@张三</p>` | `你好\n@张三`（**块级结束标签换成 `\n`**，不能直接拼成 `你好@张三`） |
| `<span data-type="mention" data-id="01H">@张三</span>` | `@张三`（标签剥掉，文字留下） |
| `<br>` | `\n` |
| `&amp;` `&lt;` `&gt;` `&quot;` `&#39;` `&nbsp;` | 解码成对应字符 |
| `<p><br></p>` | `\n` → trim 后为空 → `is_blank_body` 判空成立 |
| `<img src="...">` | 空（图片不贡献文字），但 `has_image_embed` 判它成立 |

**为什么块边界必须是 `\n`**：`mention::extract_mention_names` 要求 `@` 前面是空白或特定标点，`你好@张三` 里的 `@` 前面是汉字 → 提及不被识别 → 通知不触发。

### 4.6 存量迁移：Delta → HTML

#### 转换什么时候触发

| 路径 | 时机 | 范围 |
|---|---|---|
| **启动期 repair（主）** | `Services::new` 末尾，所有其它 repair 之后、`Ok(services)` 之前 —— **服务开始对外服务之前** | `cf::ENTRIES` + `cf::COMMENTS` 全表扫描 |
| **写入路径** | 任何一次新建/更新条目或评论（`entry.rs` / `comment.rs` 的 `normalize_for_storage`） | 单条 |
| **前端兜底（读时）** | 只读渲染时 | 单条 |

repair 的判据：`detect_format(...) == Html` → 跳过；Delta 走 `delta_to_html`、
历史纯文本走 `plain_to_html`，再 `sanitize_html` 消毒。**转换结果为空而原文非空
→ 保留原文不写**（空串等于删掉用户的正文）。批量写一次 `write_batch` 提交；
失败只 warn，不阻塞启动。

#### 只跑一次：版本标记

存量扫描是**全量**的（两张表逐条 bincode 解码 + JSON 解析），每次启动都跑一遍
在数据量大时不可接受。所以：

- 新增列族 `cf::RICH_TEXT_FORMAT`（单例，key 固定为 `format`），值是格式版本号
  （当前 `html-v1`）；
- 启动先读标记，版本对得上 → **整个扫描跳过**；
- 迁移跑完写标记（写失败下次启动重扫，幂等，安全）；
- 格式再演进一版就把版本号 +1 → 重跑。

RocksDB 侧 `create_missing_column_families(true)`，存量库打开时自动建新列族，
不需要额外迁移；Postgres 侧是单表 + `cf` 列，同样不用改 schema。

**另修一处写放大**：转换结果与原文相同（空正文这类）时跳过写回 —— 不判这一条的
话，每条空正文的记录每次启动都会被重写一遍。

#### 回滚风险（重要）

**转换不可逆。** HTML → Delta 没有反向实现，所以升级到这版之后**回滚到旧二进制
时正文会显示不出来**（数据还在库里，但旧版本认不了 HTML）。

- 升级前先备份：`cp -a data /tmp/rodeo-data-backup-$(date +%s)`；
- 第一次用新二进制启动时，日志里会有一行 `正文格式迁移完成：N 条记录转成 HTML`
  —— N 就是被转换的条数，事后能核对。



- 新模块 `src/service/richtext.rs`：
  - `pub fn delta_to_html(delta: &str) -> String` —— 把 Delta 渲染成 HTML，覆盖当前实际用到的格式：

    | Delta 形态 | HTML |
    |---|---|
    | `bold` / `italic` / `underline` / `strike` / `code` | `<strong>` / `<em>` / `<u>` / `<s>` / `<code>` |
    | `link` | `<a href rel target>` |
    | `header: 1..6` | `<h1>`..`<h6>` |
    | `list: "ordered"` / `"bullet"` | `<ol>` / `<ul>` + `<li>` |
    | `list: "checked"` / `"unchecked"` | `<ul data-type="taskList"><li data-checked="true|false">` |
    | `blockquote` | `<blockquote>` |
    | `code-block` | `<pre><code class="language-<lang>">` |
    | `code-token` | 高亮 token 的属性，落在行内文本上 —— **原样带进 HTML 的行内标签**（见下） |
    | `table-col` / `table-cell-line` | better-table 的表格结构 → `<table><tbody><tr><td>` |
    | `{"insert":{"image":"url"}}` | `<img src="url">` |
    | `{"insert":{"mention":{...}}}` | `<span data-type="mention" data-id="...">@名字</span>` |
    | 其它 | 纯文本 |

  - **`code-block` 的取值是语言字符串，不是 `true`**：fluent-editor 的 code-block blot 的 `formats()` 返回 `data-language || "plain"`。所以 `code-block: "rust"` / `"plain"` 都要认。
  - **`code-token` 是阶段 0 引入的新形态**：syntax 模块的高亮会把带 `code-token` 属性的 Delta 灌回代码块，模型因此变化。转换时要保留这些行内属性（`<span class="hljs-keyword">` 之类），否则高亮信息丢失 —— 存量转换清单**漏了它，每一段高亮代码都会被静默降级成纯文本**。
  - **`table-col` / `table-cell-line` 是 better-table 的结构属性**：漏了它，**每一张表格都会变成一堆竖线文本**。
  - **未知 op 退化成纯文本，不丢内容**（与 `flat_text` 现有的兜底策略一致）。
- 幂等 startup repair：按仓库既有模式（`service/mod.rs` 末尾那一串 `repair_*`）加 `repair_rich_text_html()`：
  - 扫 `cf::ENTRIES` 与 `cf::COMMENTS`；
  - 判定：trim 后以 `{` 开头且能解析出 `ops` 数组 → 是 Delta，转成 HTML 重写；以 `<` 开头 → 已是 HTML，跳过；其余 → 历史纯文本，包 `<p>` 后重写；
  - **只改 String 内部内容，不改 bincode 结构** → 解码兼容性不受影响；
  - 幂等：第二次跑没有可转换的记录即为空操作。
- **迁移前必须备份 `data/` 目录**（`make reset-data` 会删数据，不是备份手段）。这是唯一的回滚路径。

### 4.7 前端组件

`src/frontend/tiny_editor.rs` 的桥接形状基本不变（`window.__rodeo_tiny_editor__` → `window.__rodeo_editor__`），Rust 侧改动：

- `delta_json` 参数 → `html` 参数。
- `text-change` 事件 → Tiptap 的 `editor.on('update', ({ editor }) => editor.getHTML())`。
- `readonly` 分支 → `editable: false`。
- **分享页不再起编辑器实例**：`share.rs:169` 的 `<TinyEditor readonly>` 改成 `inner_html`，匿名页更轻。

`glue.js` 侧要新写的部分（**没有 UI 框架，自写**）：

- **工具栏**：约 12 个按钮，激活态用 `editor.isActive('bold')` 之类切 `is-active` class。
- **表格控件**：不做在工具栏里（按钮会爆炸），做**浮动条**——光标在表格内时在表格上方浮出一条（增行/增列/删行/删列/合并/拆分/删表），ProseMirror 官方示例的做法。
- **代码块语言下拉**：工具栏一个 `<select>`，作用于当前所在的代码块；Tiptap 的 `CodeBlockLowlight` 支持 `updateAttributes('language', ...)`。
- **@提及下拉**：`@tiptap/suggestion` + 自写定位（`getBoundingClientRect` + `position: fixed`），**不引 tippy.js**。
- **图片上传**：工具栏按钮（`<input type=file>`）+ `editorProps.handlePaste` 拦剪贴板里的图片文件 → 调现有的 `upload` promise 回调（Rust 侧 `upload_attachment` 逻辑不变）。
- **勾选列表**：`TaskList` / `TaskItem`，渲染出 `data-checked`。
- **链接**：`Link.configure({ openOnClick: false, autolink: true })`。

### 4.8 样式

`style/main.css:139-196+` 的 `.ql-*` 块整段删除，替换为：

- `.ProseMirror` 基础排版（与现有 `.tiny-editor` 视觉对齐）。
- 表格样式（边框、表头底色、单元格 padding、宽表横向滚动）。
- 代码块样式 + hljs token 配色（亮/暗两套）。lowlight 用 `highlight.js` 的 CSS 变量方案或直接引 `highlight.js/styles/github.css` 与 `github-dark.css`。

---

## 5. 不做的事

- **不双写**。阶段 1 一次性全量切到 HTML，不做「Delta 与 HTML 并存」的兼容期。
- **不保留 Quill Delta 的读兼容**。按仓库既有偏好（优先修旧数据，不留读时兼容层），存量由 startup repair 一次性转完。
- **不做协同编辑**。Tiptap 有 collaboration 扩展，但那是另一个量级的项目。
- **阶段 0 不引入 Tiptap**。两个阶段互不污染，阶段 0 完全可以独立交付并长期使用。
- **不改 `service/agent/tools/add_comment.rs` 的工具名与参数名**，只改 body 的格式约定（Delta JSON → 纯文本）。
- **不把 AI 对话面板的 markdown 表格渲染挪进编辑器**。两者的语法规则一致但输出目标不同（见今天那份设计稿 §6）。

---

## 6. 风险

| 风险 | 等级 | 应对 |
|---|---|---|
| 阶段 1 的消毒白名单与 Tiptap schema 不匹配 → 内容被静默吃掉 | 高 | 写死标签契约表，验证清单逐项核对 |
| 存量 Delta → HTML 转换有格式没覆盖到 → 内容丢失 | 高 | 未知 op 退化成纯文本；迁移前备份 `data/`；转换是幂等的可重复跑 |
| 只读分享页从「编辑器渲染」改成 `inner_html`，CSS 表现不一致 | 中 | 分享页的 CSS 类与编辑器正文区共用一套 |
| Tiptap 体积 | 中 | esbuild tree-shaking + `lowlight` 只引 common 语言集；产物大小在阶段 1 验收里量 |
| 私有化离线环境装不了 npm 依赖 | 中 | 产物 vendor 进仓库；`package.json` 只在开发机需要 |
| `ammonia` 默认白名单不含 `colspan` / `data-*` / `class` | 中 | 落地时先写一个最小验证：构造含表格 + mention 的 HTML 过一遍消毒，确认标签与属性都在 |
| syntax 模块的 1s debounce 高亮可能产生多余的状态变更 | 中 | 阶段 0 实测项；必要时把 `on_change` 的触发条件收紧 |
| 阶段 0 与阶段 1 的 CSS 改动重叠，做完阶段 0 再做阶段 1 会重复劳动 | 低 | 阶段 0 只加 `.ql-better-table` / syntax 相关样式；阶段 1 整段替换 |

---

## 7. 验证

按 `feedback-end-to-end-test-mandatory`：编译通过 ≠ 完成，必须跑起来用真实交互验证。

通用门槛：`make check`（native + wasm 两侧类型检查）+ `make build`。

浏览器验证用 playwright-core（`LEPTOS_SITE_ROOT=target/site-smoke`，见 `browser-verification-setup`）：

**阶段 0**

1. 工具栏出现表格按钮；hover 出行列选择器；点一下插出 3×3 表格；单元格内能打字、能换行。
2. 右键菜单有「合并单元格 / 背景色 / 删行列」。
3. 表格内容保存后刷新仍在。
4. 粘贴 markdown 表格 → 变成真表格；粘贴「带粗体的正文 + markdown 表格」→ 正文仍是粗体。
5. 往表格单元格里粘贴普通文本 → 粘进单元格，不插新表。
6. 代码块按钮 → 代码块内出现语言下拉；选 Rust/JS → 高亮生效；内容保存后刷新仍在。
7. 编辑代码块后静置 1s，观察是否产生多余的状态变更 / 保存请求。
8. 回归：图片粘贴上传、@提及、勾选列表、有序/无序列表、标题、链接。
9. 中文输入法（Safari 拼音）在列表项 / 表格单元格 / 代码块内输入都不丢字。
10. AI 对话面板：模型输出 markdown 表格 → 渲染成 `<table>`。

**阶段 1（在阶段 0 闸口判定「进入」后追加）**

11. 上述全部功能在 Tiptap 上重跑一遍。
12. 存量条目打开后正文、表格、图片、@提及都还在；迁移前后逐条比对。
13. 消毒验证：手写 `<script>alert(1)</script>`、`<img src=x onerror=alert(1)>`、`[点我](javascript:alert(1))` 提交后，库里存的是消毒后的 HTML，只读视图不执行。
14. SSR 验证：评论列表在 hydrate **之前**就有正文（现在没有）。
15. 全文检索仍能搜到表格单元格里的文字、代码块里的文字。
16. AI agent 的 `add_comment` 工具：让模型发一条带格式的评论，正文正确渲染。

---

## 8. 评审记录

- **已确认**：内容格式选 HTML（不是 ProseMirror JSON）；先阶段 0 后阶段 1，中间设决策闸口；CKEditor 因 GPL 排除；Tiptap 用 Vanilla JS 方式接入，继续走 `window.__rodeo_xxx__` 全局桥接。
- **待确认**：阶段 0 的决策闸口判据（§3.5）是否够用。
- **已修正**：初稿以为表格和语法高亮需要换编辑器才能得到 —— 实读 vendored bundle 后确认 `modules/better-table` 与 `modules/syntax` 都已随包提供，只差启用。
