# 编辑器迁移到 Tiptap v3（阶段 1）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把富文本编辑器从 `@opentiny/fluent-editor`（Quill 2.0）换成 Tiptap v3（MIT / ProseMirror），并把正文与评论的存储格式从 Quill Delta 换成**服务端消毒后的 HTML**，顺手修掉「评论正文在 SSR 里是空白的」这个洞。

**Architecture:** 继续走现有的「`public/` 下的 ESM + `window.__rodeo_xxx__` 全局桥接」模式，只是把 `fluent-editor.mjs` 换成 esbuild 打出来的 `public/tiptap/editor.mjs`，把 `glue.js` 换成 `public/tiptap/glue.js`。内容格式改 HTML 后，`service/mention.rs`、`service/search/helpers.rs`、`service/comment.rs` 里三处「在 Rust 里解析 Delta」的代码全部退休成剥标签；存量内容由一个幂等的 startup repair 一次性把 Delta 转成 HTML。

**Tech Stack:** Tiptap v3.32.0（MIT）、ProseMirror（`@tiptap/pm`）、lowlight v3、esbuild、highlight.js（通过 lowlight）、ammonia v4（HTML 消毒）、Leptos 0.8、cargo-leptos 0.3.7、playwright-core。

**Spec:** `docs/superpowers/specs/2026-10-28-editor-tiptap-migration-design.md`（§4 全部）。

> **前置条件**：本计划只在阶段 0 的闸口判定「进入阶段 1」时才执行。阶段 0 的计划与结论见 `docs/superpowers/plans/2026-10-28-editor-tables-and-syntax-highlight.md` 与 `docs/superpowers/specs/2026-10-28-editor-phase0-verdict.md`。

---

## Global Constraints

- **不新增 Rust 单元测试。** 门槛是 `make check`（native + wasm）+ `make build` + 浏览器真实交互验证。
- **迁移前必须备份 `data/` 目录**（`cp -a data /tmp/rodeo-data-backup-$(date +%s)`）。这是唯一的回滚路径 —— `make reset-data` 是删除，不是备份。
- **不双写**。一次性全量切到 HTML，不做「Delta 与 HTML 并存」的兼容期，不留读时兼容层。
- **`bincode` 结构不变**：`domain::Entry::detail` 与 `domain::Comment::body` 仍是 `String`，变的只是字符串内容。存量记录的解码不受影响。
- **编辑器静态资源 vendor 进 `public/`**，运行时零外网依赖（私有化部署是离线环境）。`node_modules` 只在开发机需要。
- **构建不要抢用户的 `:3000`**：优先用用户自己的 `cargo leptos watch`；自己构建时用 `LEPTOS_SITE_ROOT=target/site-smoke cargo leptos build`。
- **一次只跑一个 cargo 构建**（sccache 并发会死锁）。
- **每完成一个任务立即 commit**，前缀 `feat` / `fix` / `refactor` / `chore` / `docs`。不 force-push，不带 `--no-verify`。
- **改完 CSS / 前端后提醒用户「刷新一下」**（live-reload over TLS 是坏的）。

---

## Review Focus

1. **消毒白名单与 Tiptap schema 的双向契约**：白名单放行了 Tiptap 不认识的标签 → 内容被 Tiptap 的 HTML parser 静默吃掉；Tiptap 输出的标签白名单没放行 → 内容被服务端吃掉。**任一方向漏了，用户看到的就是「存的和显示的不一样」，而且没有报错。**
2. **存量迁移的不可逆性**：`repair_rich_text_html` 一旦跑过，Delta 就没了。任何「转换失败」的记录必须**保留原样**而不是写成空串 —— 空串等于删用户的正文。
3. **`@提及` 在 HTML 形态下的服务端识别**：现在 `mention::flat_text` 解析 Delta，改成剥标签后，`<span data-type="mention" data-id="01H...">@张三</span>` 必须仍然能抽出 `@张三` 去触发通知。
4. **只含图片的评论**：现在 `is_blank_body` 靠「Delta 里有 image embed」判定合法；改成 HTML 后必须仍然把「只有 `<img>` 没有文字」的评论判为**合法**。
5. **AI agent 的 `add_comment` 工具**：工具描述要求 LLM 输出正文格式。改成纯文本后，LLM 可能仍然输出旧的 Delta JSON —— 服务端必须能同时吃下「纯文本」和「Delta JSON」两种输入（判据见 Task 4）。

---

---

### Task 1: npm 工具链与 Tiptap 打包骨架

**Files:**
- Create: `package.json`
- Create: `scripts/build-editor.mjs`
- Create: `.gitignore` 追加 `node_modules/`
- Create: `public/tiptap/editor.mjs`（**构建产物，vendor 进仓库**）
- Modify: `Makefile`（新增 `editor` 目标）

**Interfaces:**
- Consumes: 无
- Produces:
  - `public/tiptap/editor.mjs` —— 一个 ESM 模块，导出 `{ Editor }`（来自 `@tiptap/core`）
  - `make editor` —— 重新生成 `public/tiptap/editor.mjs`

- [ ] **Step 1: 写 `package.json`**

```json
{
  "name": "rodeo-editor",
  "private": true,
  "type": "module",
  "scripts": {
    "build:editor": "node scripts/build-editor.mjs"
  },
  "devDependencies": {
    "@tiptap/core": "3.32.0",
    "@tiptap/pm": "3.32.0",
    "@tiptap/starter-kit": "3.32.0",
    "@tiptap/extension-image": "3.32.0",
    "@tiptap/extension-link": "3.32.0",
    "@tiptap/extension-mention": "3.32.0",
    "@tiptap/extension-placeholder": "3.32.0",
    "@tiptap/extension-table": "3.32.0",
    "@tiptap/extension-task-item": "3.32.0",
    "@tiptap/extension-task-list": "3.32.0",
    "@tiptap/extension-code-block-lowlight": "3.32.0",
    "esbuild": "0.25.0",
    "lowlight": "3.3.0"
  }
}
```

**版本必须锁死**（不用 `^`）—— 产物是 vendor 进仓库的，浮动版本会让「重新生成产物」变成不可复现的操作。

- [ ] **Step 2: 写 `scripts/build-editor.mjs`**

用 esbuild 的 JS API：

```js
import { build } from 'esbuild';
// entry: 一个临时入口文件，导出所有后续任务要用的扩展
// bundle: true, format: 'esm', minify: true, target: ['es2022']
// outfile: 'public/tiptap/editor.mjs'
```

入口文件 `scripts/editor-entry.mjs` 导出：

```js
export { Editor } from '@tiptap/core';
export { default as StarterKit } from '@tiptap/starter-kit';
export { default as TableKit } from '@tiptap/extension-table';
export { default as Image } from '@tiptap/extension-image';
export { default as Link } from '@tiptap/extension-link';
export { default as Mention } from '@tiptap/extension-mention';
export { default as Placeholder } from '@tiptap/extension-placeholder';
export { default as TaskList } from '@tiptap/extension-task-list';
export { default as TaskItem } from '@tiptap/extension-task-item';
export { default as CodeBlockLowlight } from '@tiptap/extension-code-block-lowlight';
export { common, createLowlight } from 'lowlight';
```

`lowlight` 的语言集用 `createLowlight(common)`（common 集覆盖 rust / js / ts / python / go / java / c / cpp / csharp / css / html / json / yaml / bash / sql / markdown / xml 等常用语言）。

- [ ] **Step 3: `Makefile` 加 `editor` 目标**

```makefile
editor: ## 重新生成 public/tiptap/editor.mjs（需要 node + npm install）
	npm run build:editor
```

并把它加进 `.PHONY` 那一行。

**不要**把它挂成 `build` 的前置依赖 —— `make build` 在没有 node 的机器上（CI、私有化构建机）必须仍能跑。产物是 vendor 进仓库的，只在升级 Tiptap 时手工跑一次。

- [ ] **Step 4: 构建并 vendor 产物**

```bash
cd /Users/wangxiaoyan/Works/git/rodeo
npm install
make editor
ls -lh public/tiptap/editor.mjs
```

Expected: 产物存在且是单文件 ESM。记下体积（后面要在验收里量）。

- [ ] **Step 5: 验证产物能被浏览器加载**

在 `scripts/editor-smoke.mjs` 里加一个 scenario `tiptap-bundle`：起 smoke server，在页面里 `await import('/tiptap/editor.mjs')`，断言 `Editor` 是函数，且 `new Editor({ element, extensions: [], content: '<p>hi</p>' })` 能创建、`editor.getHTML()` 返回 `<p>hi</p>`。

这一步不需要 Rust 侧任何改动 —— 只要 `public/tiptap/editor.mjs` 在 `target/site/tiptap/` 下就能被 import。

- [ ] **Step 6: Commit**

```bash
git add package.json scripts/build-editor.mjs scripts/editor-entry.mjs public/tiptap/editor.mjs Makefile .gitignore
git commit -m "build(editor): 引入 Tiptap 打包工具链并 vendor editor.mjs"
```

---

### Task 2: Rust 侧桥接改造

**Files:**
- Create: `src/frontend/rich_editor.rs`
- Modify: `src/frontend/mod.rs`（加 `pub mod rich_editor;`）
- Modify: `src/app.rs`（`<Script type_="module" src="/tiptap/glue.js">`）
- Create: `public/tiptap/glue.js`

**Interfaces:**
- Consumes: Task 1 的 `public/tiptap/editor.mjs`
- Produces:
  - JS 侧全局桥：`window.__rodeo_editor__ = { create(el, html, opts) -> editor }`，`opts = { readOnly: boolean, upload: (file: File) => Promise<string>, members: Member[] }`
  - Rust 侧组件：`src/frontend/rich_editor.rs::RichEditor`，props 与现有 `TinyEditor` 同构，但 `initial` / `on_change` 传的是 **HTML 字符串**而不是 Delta JSON

- [ ] **Step 1: 写 `public/tiptap/glue.js` 的骨架**

```js
import { Editor, StarterKit, Placeholder } from './editor.mjs';

window.__rodeo_editor__ = {
  create(el, html, opts) {
    const options = {
      element: el,
      extensions: [StarterKit, Placeholder.configure({ placeholder: '输入详情…' })],
      content: html || '',
      editable: !(opts && opts.readOnly),
    };
    const editor = new Editor(options);
    el.__rodeo_editor = editor;
    return editor;
  },
};
```

先只做到「能创建、能输入、能 getHTML」。工具栏留到 Task 6。

- [ ] **Step 2: 写 `src/frontend/rich_editor.rs`**

照搬 `src/frontend/tiny_editor.rs` 的骨架，改动点：

- 组件名 `TinyEditor` → `RichEditor`。
- `initial` 的归一化从 `normalize_delta` 改成 `normalize_html`：

```rust
/// 把正文归一化成 HTML。空内容返回空串；看起来像 HTML 的原样返回；
/// 其余视为纯文本 —— escape 后包进 `<p>`。
fn normalize_html(content: &str) -> String
```

- 桥接名 `__rodeo_tiny_editor__` → `__rodeo_editor__`；调用的方法从 `getContents()` 改成 `getHTML()`。
- 事件从 Quill 的 `text-change` 改成 Tiptap 的 `update`：

```js
editor.on('update', ({ editor }) => { ... on_change.run(editor.getHTML()) })
```

- **保持 `on_uploaded` 的语义不变**：上传成功后必须回调宿主重新取条目（`tiny_editor.rs:20-25` 的注释解释了为什么：上传推进了 `updated_at`，乐观并发版本号作废）。这一步不能省。

- [ ] **Step 3: 接到调用点**

把 4 处 `<TinyEditor>` 换成 `<RichEditor>`：

- `src/frontend/pages/workspace_main.rs:4757`
- `src/frontend/pages/entry.rs:391`
- `src/frontend/pages/share.rs:169`
- `src/frontend/comment_list.rs:322` 与 `:386`

**注意**：此时存储还是 Delta（Task 3/5 才切），所以 `initial` 传进来的仍是 Delta JSON。先让 `normalize_html` 兼容 Delta —— 不，**不要**。改为：**Task 2 阶段先不改存储，`normalize_html` 收到 Delta JSON 时原样返回（内容暂时显示不对），Task 3–5 完成后一起验证**。

更干净的做法：**把 Task 3–5（存储格式）排在这个任务之前**，让 `RichEditor` 一开始就只认 HTML。本计划按 **Task 3 → Task 4 → Task 5 → Task 2** 的顺序执行，Task 编号保留设计顺序但注明依赖。实际执行顺序以「Interfaces 里的 Consumes」为准。

- [ ] **Step 4: 类型检查**

Run: `make check`
Expected: native 与 wasm 两侧通过。

- [ ] **Step 5: Commit**

```bash
git add src/frontend/rich_editor.rs src/frontend/mod.rs src/app.rs public/tiptap/glue.js
git commit -m "feat(editor): Rust 侧桥接切到 Tiptap，正文出入改为 HTML"
```

---

### Task 3: 服务端 HTML 消毒

**Files:**
- Create: `src/service/richtext.rs`
- Modify: `src/service/mod.rs`（`pub mod richtext;`）
- Modify: `Cargo.toml`（加 `ammonia = "4.2"`）
- Modify: `src/service/entry.rs`（`create_blank_impl` / `update` 写入前消毒）
- Modify: `src/service/comment.rs`（`create` / `update` 写入前消毒）

**Interfaces:**
- Consumes: 无
- Produces: `src/service/richtext.rs` 里的

```rust
/// 把 HTML 消毒成安全子集。输入非法 HTML 时返回空串。
pub fn sanitize_html(html: &str) -> String

/// 剥掉所有标签，取纯文本（保留 mention chip 里的文字）。
pub fn html_to_text(html: &str) -> String
```

- [ ] **Step 1: 读 ammonia 的 API**

```bash
ls ~/.cargo/registry/src/index.crates.io-*/ammonia-4.2.4/src/
grep -n "pub fn " ~/.cargo/registry/src/index.crates.io-*/ammonia-4.2.4/src/lib.rs | head -40
```

把 `Builder` 上的 `tags` / `generic_attributes` / `generic_attribute_prefixes` / `url_schemes` / `link_rel` / `allow_relative_links` 的确切签名抄下来 —— 下一步要用。

- [ ] **Step 2: 写 `sanitize_html`**

白名单（**这张表同时是 Tiptap schema 的契约**，Task 6–11 输出的标签必须全在表内）：

| 标签 | 允许的属性 |
|---|---|
| `p` `br` `hr` `div` `span` `strong` `em` `u` `s` `code` `pre` `blockquote` `h1` `h2` `h3` `h4` `h5` `h6` `ul` `ol` `li` `a` `img` `table` `thead` `tbody` `tfoot` `tr` `th` `td` | 见下 |
| `a` | `href` `rel` `target` |
| `img` | `src` `alt` `title` `width` `height` |
| `th` `td` | `colspan` `rowspan` `align` |
| `code` `pre` | `class`（只放行 `language-*`） |
| `li` | `data-checked` `data-type` |
| `span` | `data-type` `data-id` `data-label` `data-checked` |

必须**禁止**：`style`、`on*` 事件属性、`javascript:` / `data:` URL、`<script>` / `<style>` / `<iframe>` / `<object>` / `<embed>`。

`data-*` 用 ammonia 的通用属性前缀机制放行；`class` 只在 `code` / `pre` 上放行。

- [ ] **Step 3: 写 `html_to_text`**

剥标签取纯文本。**规则必须写死** —— 这一条同时决定 `mention::flat_text`、`search::strip_rich_text`、`comment::is_blank_body`、AI 摘要输入四个行为：

| 输入 | 输出 |
|---|---|
| `<p>你好</p><p>@张三</p>` | `你好\n@张三` |
| `<span data-type="mention" data-id="01H">@张三</span>` | `@张三`（标签剥掉，文字留下） |
| `<br>` | `\n` |
| `&amp;` `&lt;` `&gt;` `&quot;` `&#39;` `&nbsp;` | 解码成对应字符 |
| `<p><br></p>` | `\n` → trim 后为空 → `is_blank_body` 判空成立 |
| `<img src="...">` | 空（图片不贡献文字），但 `has_image_embed` 判它成立 |
| 不是 HTML（历史纯文本） | 原样返回 |

**块边界必须是 `\n`，不能直接拼**：`mention::extract_mention_names` 要求 `@` 前面是空白或特定标点，`你好@张三` 里的 `@` 前面是汉字 → 提及不被识别 → **通知不触发**。

- [ ] **Step 4: 接到写入点**

`src/service/entry.rs`：

```rust
// create_blank_impl
entry.detail = crate::service::richtext::sanitize_html(detail);
// update
entry.detail = crate::service::richtext::sanitize_html(detail);
```

`src/service/comment.rs`：

```rust
// create
let body = crate::service::richtext::sanitize_html(body);
// update
comment.body = crate::service::richtext::sanitize_html(body);
```

**注意**：`is_blank_body` 的判定在消毒**之后**做（顺序不能反）—— 消毒可能把一个只剩 `<script>` 的正文变成空串，这时应该判为空并拒绝。

- [ ] **Step 5: 类型检查**

Run: `make check`
Expected: 通过。

- [ ] **Step 6: 浏览器验证 —— 消毒生效**

在浏览器里用一个条目提交含以下内容的正文，保存后读库（GraphQL 查条目）确认存的是消毒后的 HTML：

- `<script>alert(1)</script>` → 不存在
- `<img src=x onerror=alert(1)>` → `onerror` 不存在
- `<a href="javascript:alert(1)">点我</a>` → `href` 被清掉或改成安全值
- `<table><tr><td colspan="2">x</td></tr></table>` → `colspan` **保留**

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/service/richtext.rs src/service/mod.rs src/service/entry.rs src/service/comment.rs
git commit -m "feat(richtext): 正文写入前做服务端 HTML 消毒"
```

---

### Task 4: Rust 侧 Delta 依赖退休

**Files:**
- Modify: `src/service/mention.rs`（`flat_text` / `preview`）
- Modify: `src/service/search/helpers.rs`（`strip_rich_text`）
- Modify: `src/service/comment.rs`（`is_blank_body` / `has_image_embed`）
- Modify: `src/service/agent/tools/add_comment.rs`（工具描述 + 参数处理）

**Interfaces:**
- Consumes: Task 3 的 `richtext::html_to_text`
- Produces: 签名不变，语义改变

```rust
// mention.rs
pub fn flat_text(html: &str) -> String        // 原来收 Delta，现在收 HTML
pub fn preview(html: &str, max_chars: usize) -> String

// search/helpers.rs
pub fn strip_rich_text(html: &str) -> String   // 原来收 Delta，现在收 HTML

// comment.rs
fn is_blank_body(html: &str) -> bool          // 纯文本判空；含 <img> 算合法
fn has_image_embed(html: &str) -> bool        // 查 <img
```

- [ ] **Step 1: `mention.rs` 换实现**

`flat_text` 改成调用 `richtext::html_to_text`。**但要兼容两种输入**：AI agent 仍可能吐旧的 Delta JSON（Review Focus 第 5 条）。做法：

```rust
pub fn flat_text(content: &str) -> String {
    let c = content.trim();
    if c.is_empty() { return String::new(); }
    // 旧格式：Quill Delta JSON
    if c.starts_with('{') || c.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(c) {
            if let Some(ops) = v.get("ops").and_then(|o| o.as_array()).or_else(|| v.as_array()) {
                // 走原来的 Delta 抽文本逻辑
            }
        }
    }
    crate::service::richtext::html_to_text(c)
}
```

**Delta 抽文本的旧逻辑保留在函数内部，不要另开模块** —— 它只服务兼容期。

- [ ] **Step 2: `search/helpers.rs` 换实现**

`strip_rich_text` 同样改成「先试 Delta，再试 HTML 剥标签」。

**注意**：`strip_rich_text` 被 `service/ai.rs:487`（AI 摘要输入）、`service/comment.rs:266`、`service/search/meilisearch.rs:130` 共用。改成 HTML 后 AI 摘要的输入不再带 Delta 的换行结构，**摘要质量可能变化** —— 这是预期的（HTML 的块级换行现在会变成 `\n`）。

- [ ] **Step 3: `comment.rs` 换实现**

- `is_blank_body`：剥标签后为空 **且** 不含 `<img` → 空。
- `has_image_embed`：`<img` 子串（大小写不敏感）。**图片 embed 在 HTML 里就是 `<img src=...>`**。

- [ ] **Step 4: agent 工具改成纯文本**

`src/service/agent/tools/add_comment.rs`：

- 工具描述里的「正文是 Quill Delta JSON 字符串，与 Entry.detail 同构」改成「正文是纯文本（可含换行）。服务端会转成 HTML 并消毒」。

`src/service/agent/tools/update_entry.rs:92`：

- `detail` 参数描述同样写着「Quill Delta JSON 字符串」—— **这条和 add_comment 是同一类问题，一起改**。

**放在哪一层**：不要让 agent 工具自己拼 HTML —— 让服务层接受纯文本并负责包装，这样 UI 侧和 agent 侧走同一条路。

具体做法：在 `src/service/richtext.rs` 里加一个统一的收口函数：

```rust
/// 正文写入前的统一归一化：已经是 HTML 就消毒；否则当纯文本按行包 <p> 后消毒。
pub fn normalize_for_storage(content: &str) -> String
```

`CommentService::create` / `update` 与 `EntryService::create_blank_impl` / `update` 都走它（Task 3 已经接了 `sanitize_html`，这里把「纯文本 → HTML」这一步也收进来）。

**判断「已经是 HTML」**：`trim()` 后以 `<` 开头 → 当 HTML；否则当纯文本。历史 Delta（以 `{` 开头）也要认 —— 存量由 Task 5 的 startup repair 处理，但在这之前（repair 跑过、某个入口又被写了一次）仍可能有 Delta 混进来，所以 `normalize_for_storage` 里对以 `{`/`[` 开头的走 `delta_to_html`。

- [ ] **Step 4b: AI 摘要的正文写入**

`src/api/graphql.rs:2615` 的 `summarizeEntries` 把 AI 生成的 **Markdown 纯文本** summary 直接 `create_with_detail(..., &summary)` 写进 `entry.detail` —— 既不是 Delta 也不是 HTML。

改成走 `normalize_for_storage`（纯文本 → 按行包 `<p>`）。

**注意**：摘要是 Markdown（可能有 `**加粗**`、`` `代码` ``）。`normalize_for_storage` 当纯文本处理 → Markdown 标记会原样显示成字面文本。这与现状一致（现在也是原样存 Delta 里的文本），不是回退；但如果希望摘要按 Markdown 渲染，需要另加一个 markdown→HTML 的转换。**本次不做**（YAGNI），在代码注释里写明。

- [ ] **Step 5: 类型检查**

Run: `make check`
Expected: 通过。

- [ ] **Step 6: 浏览器验证 —— @提及与检索没坏**

- 发一条带 `@姓名` 的评论 → 收件箱/通知里出现该提及（`service/mention.rs::resolve_mentions` 路径）。
- 全文检索能搜到评论正文里的文字、表格单元格里的文字。
- AI agent 的 `add_comment` 工具发一条评论 → 评论列表里正常显示。

- [ ] **Step 7: Commit**

```bash
git add src/service/mention.rs src/service/search/helpers.rs src/service/comment.rs src/service/agent/tools/add_comment.rs
git commit -m "refactor(richtext): Delta 依赖退休，mention/检索/评论改吃 HTML"
```

---

### Task 5: 存量 Delta → HTML 迁移

**Files:**
- Modify: `src/service/richtext.rs`（加 `delta_to_html` / `detect_format`）
- Modify: `src/service/mod.rs`（加 `repair_rich_text_html` 调用）
- Modify: `src/service/entry.rs` / `src/service/comment.rs`（加各自的 repair 方法）

**Interfaces:**
- Consumes: Task 3 的 `sanitize_html`
- Produces: `src/service/richtext.rs` 里的

```rust
/// 内容是 Delta JSON、HTML 还是历史纯文本。
pub enum RichFormat { Delta, Html, Plain }

/// 判定内容格式。Delta = trim 后以 { 或 [ 开头且能解析出 ops 数组。
pub fn detect_format(content: &str) -> RichFormat

/// 把 Quill Delta 渲染成 HTML。未知 op 退化成纯文本，不丢内容。
pub fn delta_to_html(delta: &str) -> String
```

- [ ] **Step 1: 写 `detect_format`**

```rust
pub enum RichFormat { Delta, Html, Plain }

pub fn detect_format(content: &str) -> RichFormat {
    let c = content.trim();
    if c.is_empty() { return RichFormat::Plain; }
    if c.starts_with('<') { return RichFormat::Html; }
    if c.starts_with('{') || c.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(c) {
            let has_ops = v.get("ops").and_then(|o| o.as_array()).is_some() || v.as_array().is_some();
            if has_ops { return RichFormat::Delta; }
        }
    }
    RichFormat::Plain
}
```

- [ ] **Step 2: 写 `delta_to_html`**

映射表（**每一条都要用真实存量数据核对**）：

| Delta 形态 | 输出 HTML |
|---|---|
| `{"insert":"文本","attributes":{"bold":true}}` | `<strong>文本</strong>` |
| `italic` | `<em>文本</em>` |
| `underline` | `<u>文本</u>` |
| `strike` | `<s>文本</s>` |
| `code` | `<code>文本</code>` |
| `link` | `<a href="..." rel="noopener noreferrer" target="_blank">文本</a>` |
| `header: 1..6` | `<h1>`..`<h6>` |
| `list: "ordered"` | `<ol><li>…</li></ol>` |
| `list: "bullet"` | `<ul><li>…</li></ul>` |
| `list: "checked"` / `"unchecked"` | `<ul data-type="taskList"><li data-checked="true|false">…</li></ul>` |
| `blockquote` | `<blockquote>…</blockquote>` |
| `code-block: "<语言>"` | `<pre><code class="language-<语言>">…</code></pre>` |
| `code-token: "<token>"` | 行内 token，保留（`<span class="hljs-<token>">`） |
| `table-col` / `table-cell-line` | `<table><tbody><tr><td>…</td></tr></tbody></table>` |
| `{"insert":{"image":"url"}}` | `<img src="url">` |
| `{"insert":{"mention":{"char":"@","text":"张三","name":"<id>"}}}` | `<span data-type="mention" data-id="<id>">@张三</span>` |
| 其它 `{"insert":"文本"}` | 纯文本 |
| 未知 op | 纯文本（**不丢内容**） |

**三个容易漏的形态**（漏了就是静默丢内容）：

1. **`code-block` 的取值是语言字符串，不是 `true`** —— fluent-editor 的 code-block blot 的 `formats()` 返回 `data-language || "plain"`。`code-block: "rust"` / `"plain"` 都要认。
2. **`code-token` 是阶段 0 引入的** —— syntax 模块的高亮会把带 `code-token` 属性的 Delta 灌回代码块，模型因此变化。漏了它，**每一段高亮代码都降级成纯文本**。
3. **`table-col` / `table-cell-line` 是 better-table 的结构属性** —— 漏了它，**每一张表格都变成一堆竖线文本**。

**列表的 Delta 形状有两个**：`{"list":"ordered"}`（Quill 标准）与 `{"list":{"value":"ordered"}}`（fluent-editor 改过的）—— **两种都要认**。

**分段规则**：Delta 的 `insert` 里带 `\n` 的文本表示块边界。渲染时按 `\n` 切段，段内的行内属性（bold/italic/link…）转成对应的行内 HTML，块级属性（header/list/blockquote/code-block）决定外层标签。

**产出统一过一遍 `sanitize_html`** —— 转换结果不该绕过消毒。

- [ ] **Step 3: 写 repair 方法**

`src/service/entry.rs`：

```rust
/// 幂等：把存量条目正文从 Delta / 纯文本转成 HTML。
pub fn repair_rich_text_html(&self) -> Result<usize, AppError>
```

实现（照 `src/service/relation.rs:326` 的 `repair_undecodable` 形状）：

```rust
let rows = self.store.scan_prefix(cf::ENTRIES, b"")?;
let mut ops: Vec<BatchOp> = Vec::new();
for (k, v) in &rows {
    let Ok(entry) = bincode::deserialize::<Entry>(v) else { continue };
    let html = match richtext::detect_format(&entry.detail) {
        richtext::RichFormat::Delta => richtext::delta_to_html(&entry.detail),
        richtext::RichFormat::Plain => richtext::plain_to_html(&entry.detail),
        richtext::RichFormat::Html => continue,   // 已经是 HTML，幂等跳过
    };
    if html == entry.detail { continue; }
    let mut e = entry;
    e.detail = html;
    ops.push(BatchOp::put(cf::ENTRIES, k.clone(), &e));
}
self.store.batch(ops)?;
```

`src/service/comment.rs` 同理，扫 `cf::COMMENTS`（key = `comment_key(entry_code, comment_id)`）。

**`plain_to_html` 也要写**：

```rust
/// 历史纯文本 → HTML：按行切分，每行包 <p>，空行跳过。文本要 escape。
pub fn plain_to_html(text: &str) -> String
```

- [ ] **Step 4: 接到启动流程**

`src/service/mod.rs` 的 `Services::new` 末尾那一串 repair 里加：

```rust
// 正文格式：Delta / 历史纯文本 → HTML。幂等，已是 HTML 的跳过。
if let Err(e) = services.entry.repair_rich_text_html() {
    tracing::warn!("条目正文格式启动迁移失败（不阻塞启动）: {e}");
}
if let Err(e) = services.comment.repair_rich_text_html() {
    tracing::warn!("评论正文格式启动迁移失败（不阻塞启动）: {e}");
}
```

**不阻塞启动**（跟 `repair_orphan_index_rows` 一样用 warn 而不是 `?`）—— 迁移失败不该让整个服务起不来。

- [ ] **Step 5: 备份数据目录**

```bash
cd /Users/wangxiaoyan/Works/git/rodeo
cp -a data /tmp/rodeo-data-backup-$(date +%s)
ls -d /tmp/rodeo-data-backup-*   # 记下这个路径
```

**这一步必须在跑 repair 之前做。**

- [ ] **Step 6: 类型检查 + 跑一次迁移**

```bash
make check
make build
```

启动一次服务（`./target/debug/rodeo /tmp/rodeo-smoke.toml`），看启动日志里有没有 repair 的 warn。停掉，再启动一次 —— **第二次应当没有任何转换发生**（幂等）。

- [ ] **Step 7: 浏览器验证 —— 迁移前后逐条比对**

1. 迁移**前**用 GraphQL 拉一批条目的 `detail` 与全部评论的 `body`，存到 `/tmp/before.json`。
2. 跑迁移。
3. 迁移**后**再拉一遍，存 `/tmp/after.json`。
4. 对每条比对：**文字内容必须完全一致**（去掉标签后的纯文本相同），差异只能出现在格式（`<strong>` vs 纯文本）。

脚本写成一个临时 node/python 脚本即可，不必进仓库。

- [ ] **Step 8: Commit**

```bash
git add src/service/richtext.rs src/service/mod.rs src/service/entry.rs src/service/comment.rs
git commit -m "feat(richtext): 存量 Delta/纯文本 正文幂等迁移成 HTML"
```

---

### Task 6: Tiptap 编辑器装配 —— 基础格式与工具栏

**Files:**
- Modify: `public/tiptap/glue.js`
- Create: `public/tiptap/style.css`
- Modify: `src/app.rs`（`<Stylesheet id="tiny-editor" ...>` 换成新的）

**Interfaces:**
- Consumes: Task 1 的 `public/tiptap/editor.mjs`
- Produces: `window.__rodeo_editor__.create(el, html, opts)` 返回的 editor 带完整工具栏

- [ ] **Step 1: 写工具栏 DOM**

工具栏是**自写的 HTML**（不引 UI 框架），由 `glue.js` 在 `create()` 里插到宿主容器**之前**（与现在 Quill 的做法一致：工具栏是容器的兄弟节点）。

结构：

```
[ B | I | S | <u> | `code` ] [ H1 | H2 | H3 ] [ • | 1. | ☑ ] [ ❝ | </> ] [ 🔗 | 🖼 ] [ ↶ | ↷ ]
```

每个按钮 `data-cmd="toggleBold"` 之类；激活态用 `editor.isActive('bold')` 切 `is-active` class。

**不要用 Quill 的 `.ql-toolbar` 类名** —— `style/main.css` 里那套 `.ql-*` 规则会在 Task 12 整段删掉。

- [ ] **Step 2: 装配扩展集**

```js
const extensions = [
  StarterKit.configure({ codeBlock: false }),   // 用 lowlight 版
  TableKit.configure({ resizable: true }),
  Image.configure({ allowBase64: false }),
  Link.configure({ openOnClick: false, autolink: true }),
  TaskList,
  TaskItem.configure({ nested: true }),
  CodeBlockLowlight.configure({ lowlight, defaultLanguage: 'plain' }),
  Placeholder.configure({ placeholder: '输入详情…' }),
];
```

`Mention` 留到 Task 9（要先写 suggestion 渲染器）。

- [ ] **Step 3: 写 `public/tiptap/style.css` 的最小版本**

ProseMirror 基础排版：`.ProseMirror` 的字号/行高/间距、`p` 间距、`blockquote` 左边框、`pre` 背景、`h1`-`h3` 字号。**亮/暗两套**（跟现有 `--bg` / `--fg` 变量走）。

- [ ] **Step 4: 验证**

scenario `tiptap-basic`：在编辑器里输入文字、加粗、设标题、插有序/无序列表、插引用、插代码块、插链接、撤销重做。逐个断言 DOM 里有对应元素。

- [ ] **Step 5: Commit**

```bash
git add public/tiptap/glue.js public/tiptap/style.css src/app.rs
git commit -m "feat(editor): Tiptap 工具栏与基础格式"
```

---

### Task 7: 表格

**Files:**
- Modify: `public/tiptap/glue.js`
- Modify: `public/tiptap/style.css`

**Interfaces:**
- Consumes: Task 6 的工具栏框架
- Produces: 表格插入 + **浮动工具条**（光标在表格内时浮在表格上方）

- [ ] **Step 1: 表格浮动条**

工具栏放不下 7 个表格按钮。做法：光标进入表格时，在表格上方浮出一条（`position: absolute`，锚定表格的 `getBoundingClientRect()`）。按钮：增行 / 增列 / 删行 / 删列 / 合并单元格 / 拆分单元格 / 删除表格。

命令（`@tiptap/extension-table` 的 `TableKit` 提供）：

```js
editor.chain().focus().addRowAfter().run()
editor.chain().focus().addColumnAfter().run()
editor.chain().focus().deleteRow().run()
editor.chain().focus().deleteColumn().run()
editor.chain().focus().mergeCells().run()
editor.chain().focus().splitCell().run()
editor.chain().focus().deleteTable().run()
```

浮动条的显示由 `editor.on('selectionUpdate')` 驱动：判断当前选区在表格内（`editor.isActive('table')`）。

- [ ] **Step 2: 表格样式**

`public/tiptap/style.css` 加：边框、表头底色、单元格 padding、宽表横向滚动（`overflow-x: auto`）。

- [ ] **Step 3: 验证**

scenario `tiptap-table`：插 3×3 表格 → 填字 → 合并单元格 → 增行 → 保存 → 刷新 → 断言结构与内容都对。

**关键断言**：`colspan` 在存储的 HTML 里保留（因为 sanitizer 放行了它），刷新后仍然合并着。

- [ ] **Step 4: Commit**

```bash
git add public/tiptap/glue.js public/tiptap/style.css
git commit -m "feat(editor): Tiptap 表格与浮动工具条"
```

---

### Task 8: 代码块与语法高亮

**Files:**
- Modify: `public/tiptap/glue.js`
- Modify: `public/tiptap/style.css`

**Interfaces:**
- Consumes: Task 1 的 `common` / `createLowlight`
- Produces: 代码块 + 语言下拉

- [ ] **Step 1: 语言下拉**

工具栏放一个 `<select>`，列出 `lowlight` 的 common 语言集。作用于**当前所在的代码块**：

```js
const lang = select.value;
editor.chain().focus().updateAttributes('codeBlock', { language: lang }).run();
```

- [ ] **Step 2: 样式**

代码块背景、等宽字体、token 配色（亮/暗两套）。可以直接用 lowlight 的 class 名（`hljs-keyword` 等）配一套自己的配色变量。

- [ ] **Step 3: 验证**

scenario `tiptap-codeblock`：插入代码块 → 选 Rust → 断言有 token span → 保存 → 刷新 → 断言 `<pre><code class="language-rust">` 且高亮仍在。

- [ ] **Step 4: Commit**

```bash
git add public/tiptap/glue.js public/tiptap/style.css
git commit -m "feat(editor): Tiptap 代码块与语法高亮"
```

---

### Task 9: @提及

**Files:**
- Modify: `public/tiptap/glue.js`

**Interfaces:**
- Consumes: Task 6 的编辑器装配
- Produces: 输入 `@` 弹出成员下拉，选中后插入 mention chip

- [ ] **Step 1: 装配 Mention 扩展**

```js
Mention.configure({
  HTMLAttributes: { class: 'mention' },
  suggestion: {
    items: ({ query }) => members.filter(m => m.name.toLowerCase().includes(query.toLowerCase())).slice(0, 8),
    render: () => { /* 自写下拉，见 Step 2 */ },
  },
})
```

**注意**：Quill 版是「Rust 侧传原始名单，JS 侧闭包搜索」（因为 fluent-editor 的 mention 模块只认 `search(term)`）。Tiptap 的 `suggestion.items` 是标准的，直接在 JS 侧过滤即可。

- [ ] **Step 2: 自写下拉渲染器**

**不引 tippy.js**。用 `clientRect`（Tiptap 的 suggestion 回调提供）定位：

```js
render() {
  let el, items;
  return {
    onStart(props) { el = document.createElement('div'); el.className = 'mention-menu'; document.body.appendChild(el); items = props; this.position(props.clientRect); },
    onUpdate(props) { items = props; this.position(props.clientRect); },
    onKeyDown({ event }) { /* ↑↓ 选中、Enter 确认、Esc 关闭 */ },
    onExit() { el?.remove(); },
  };
}
```

`position(clientRect)` 用 `clientRect.left` / `clientRect.bottom` + `position: fixed` 定位。

- [ ] **Step 3: 验证**

scenario `tiptap-mention`：输入 `@` → 断言下拉出现 → 选一个成员 → 断言插入的是 `<span data-type="mention" data-id="...">`，且文本是 `@姓名`。

保存后读库确认 `data-id` 与 `data-type` 都在（sanitizer 放行了 `data-*`）。

- [ ] **Step 4: Commit**

```bash
git add public/tiptap/glue.js
git commit -m "feat(editor): Tiptap @提及下拉"
```

---

### Task 10: 图片上传

**Files:**
- Modify: `public/tiptap/glue.js`

**Interfaces:**
- Consumes: `opts.upload(file) -> Promise<string>`（Rust 侧已有的 `upload_attachment` 调用）
- Produces: 粘贴图片与工具栏选文件都能插图

- [ ] **Step 1: 粘贴上传**

```js
editorProps: {
  handlePaste(view, event) {
    const files = [...(event.clipboardData?.files || [])].filter(f => (f.type || '').startsWith('image/'));
    if (!files.length) return false;
    event.preventDefault();
    uploadFile(files[0]);
    return true;
  },
}
```

`uploadFile` 调 `opts.upload(file)`，成功后 `editor.chain().focus().setImage({ src: url }).run()`。

**必须调用 `opts.onUploaded`（Rust 侧的 `on_uploaded` 回调）** —— 上传推进了条目的 `updated_at`，宿主的乐观并发版本号作废，不回调会导致紧接着的保存被判成冲突（`tiny_editor.rs:20-25` 的注释）。

- [ ] **Step 2: 工具栏按钮**

一个隐藏的 `<input type="file" accept="image/*">`，按钮触发它。

- [ ] **Step 3: 验证**

scenario `tiptap-image`：粘贴一张图 → 断言上传请求发出 → 图片出现在编辑器里 → 保存 → 刷新 → 图片还在（`src` 是 `/api/attachments/...` 的免鉴权 URL）。

**额外断言**：上传完成后紧接着点「保存」，**不能报冲突**。

- [ ] **Step 4: Commit**

```bash
git add public/tiptap/glue.js
git commit -m "feat(editor): Tiptap 图片粘贴与选择上传"
```

---

### Task 11: 任务列表与链接

**Files:**
- Modify: `public/tiptap/glue.js`

**Interfaces:**
- Consumes: Task 6 的工具栏框架
- Produces: 勾选列表与链接

- [ ] **Step 1: 任务列表**

工具栏加「任务列表」按钮 → `editor.chain().focus().toggleTaskList().run()`。

渲染出来是 `<ul data-type="taskList"><li data-checked="false">`。

- [ ] **Step 2: 链接**

点「链接」按钮 → `window.prompt('链接地址')` → 有值则 `setLink({ href })`，没有则 `unsetLink()`。

**`openOnClick: false`**（Task 6 已配）—— 编辑态点击不应该跳走。

- [ ] **Step 3: 验证**

scenario `tiptap-tasklink`：插一个任务列表 → 勾选第一个 → 断言 `data-checked="true"` → 保存 → 刷新 → 断言勾选状态还在。

插一个链接 → 断言 `<a href>` 且 `target="_blank" rel="noopener noreferrer"`。

- [ ] **Step 4: Commit**

```bash
git add public/tiptap/glue.js
git commit -m "feat(editor): Tiptap 任务列表与链接"
```

---

### Task 12: 样式替换与旧资源清理

**Files:**
- Modify: `style/main.css`（删掉 `.ql-*` 那段，补 Tiptap 的样式）
- Delete: `public/tiny-editor/fluent-editor.mjs`
- Delete: `public/tiny-editor/style.css`
- Delete: `public/tiny-editor/glue.js`
- Delete: `public/tiny-editor/hljs.min.js`
- Modify: `src/app.rs`（删掉 tiny-editor 的 Stylesheet / Script）
- Modify: `src/frontend/tiny_editor.rs`（删除）与 `src/frontend/mod.rs`

**Interfaces:**
- Consumes: Task 6–11 的全部产出
- Produces: 仓库里不再有 Quill 的痕迹

- [ ] **Step 1: 把 Tiptap 的样式搬进 `style/main.css`**

`public/tiptap/style.css` 的内容并进 `style/main.css`（因为 `style-file = "style/main.css"` 会被 cargo-leptos 打进 `pkg/rodeo.css`）。

`style/main.css:139-196+` 的 `.ql-*` 整段删掉，换成对应的 Tiptap / ProseMirror 类名。

**注意**：删 `.ql-*` 之前先确认没有别的地方依赖它 —— `grep -rn "ql-" style/main.css src/`。

- [ ] **Step 2: 删旧资源**

```bash
git rm public/tiny-editor/fluent-editor.mjs public/tiny-editor/style.css public/tiny-editor/glue.js public/tiny-editor/hljs.min.js
```

**`public/tiny-editor/` 目录会空掉**，一起删。

- [ ] **Step 3: 删 Rust 侧的旧组件**

删 `src/frontend/tiny_editor.rs`，从 `src/frontend/mod.rs` 移除 `pub mod tiny_editor;`。

**但 `delta_to_html` 的调用点已经改成直接用存储的 HTML**（Task 5 之后），所以 `comment_list.rs:143` 与 `share.rs:62` 改用 `inner_html=body`（存储的 HTML 已在写入时消毒）。

- [ ] **Step 4: 类型检查 + 构建**

```bash
make check
make build
```

- [ ] **Step 5: 浏览器验证 —— 亮/暗两套样式都对**

scenario `tiptap-theme`：在亮色与暗色下各截一次编辑器区域，确认工具栏按钮、表格边框、代码块背景都可见（对比度够）。

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(editor): 删除 Quill 资源，样式切到 Tiptap"
```

---

### Task 13: 只读路径与分享页

**Files:**
- Modify: `src/frontend/pages/share.rs`（`:62` 与 `:169`）
- Modify: `src/frontend/comment_list.rs`（`:143`）

**Interfaces:**
- Consumes: Task 5 之后的存储格式（HTML）
- Produces: 分享页与评论列表**不再起编辑器实例**，直接渲染 HTML

- [ ] **Step 1: 分享页正文改 `inner_html`**

`src/frontend/pages/share.rs:169` 的 `<TinyEditor readonly ...>` 换成：

```rust
<div class="c-body" inner_html=entry.detail></div>
```

**匿名分享页因此不再加载编辑器** —— 分享页是公开页面，少加载 460KB 的编辑器是实打实的收益。

- [ ] **Step 2: 评论正文改 `inner_html`**

`src/frontend/comment_list.rs:143` 与 `src/frontend/pages/share.rs:62` 已经是 `inner_html=delta_to_html(&comment.body)`，改成 `inner_html=comment.body`（存储的就是消毒过的 HTML）。

**这一步同时修掉「评论正文在 SSR 里是空白的」** —— `delta_to_html` 是 wasm-only，SSR 阶段返回空串。

- [ ] **Step 3: 验证**

scenario `tiptap-share`：

1. 建一个带正文（段落 + 表格 + 代码块 + 图片 + @提及）的条目，拿到分享链接。
2. **在关掉 JS 的上下文里**（`context = browser.newContext({ javaScriptEnabled: false })`）打开分享链接，断言正文**完整可见**（表格、代码块、图片都在）。

   **这是「SSR 能渲染」的硬证据。**

- [ ] **Step 4: Commit**

```bash
git add src/frontend/pages/share.rs src/frontend/comment_list.rs
git commit -m "feat(editor): 分享页与评论列表直接渲染 HTML，SSR 就能出正文"
```

---

### Task 14: 全量回归

**Files:**
- Modify: `docs/superpowers/specs/2026-10-28-editor-phase0-verdict.md`（追加阶段 1 的结论）

**Interfaces:**
- Consumes: Task 1–13 的全部产出
- Produces: 阶段 1 的验收结论

- [ ] **Step 1: 跑设计稿 §7「阶段 1」的验证清单**

11–16 条逐条跑：

11. 阶段 0 的全部功能在 Tiptap 上重跑一遍。
12. 存量条目打开后正文、表格、图片、@提及都还在；迁移前后逐条比对（Task 5 Step 7 已做过一次，这里再抽查 5 条）。
13. 消毒验证：`<script>` / `onerror` / `javascript:` 三种载荷提交后库里是消毒后的 HTML，只读视图不执行。
14. SSR 验证：评论列表在 hydrate **之前**就有正文。
15. 全文检索仍能搜到表格单元格里的文字、代码块里的文字。
16. AI agent 的 `add_comment` 工具发评论，正文正确渲染。

- [ ] **Step 2: 回归 Quill 时代的功能**

- 图片粘贴上传（含上传后立刻保存不报冲突）
- @提及（前端下拉 + 服务端通知）
- 勾选列表
- 有序/无序列表（**Safari 拼音输入列表项不被吃掉** —— 旧版靠 `glue.js` 的 `attachUI` 补丁，Tiptap 的 ProseMirror 内核本来就没这个 bug，但要实测确认）
- 标题、链接、代码块
- 只读分享页

- [ ] **Step 3: 写结论**

在 `docs/superpowers/specs/2026-10-28-editor-phase0-verdict.md` 里追加「阶段 1 验收」一节，逐条记结果。

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-10-28-editor-phase0-verdict.md
git commit -m "docs(editor): 阶段 1 验收结论"
```

---

## 自检

- **规范覆盖**：设计稿 §4.1（选型）→ Task 1；§4.2（打包）→ Task 1；§4.3（HTML 存储）→ Task 3/4/5；§4.4（消毒）→ Task 3；§4.5（Rust 侧 7 处 Delta 依赖）→ Task 4（7 处逐条对应：mention.rs ×2、search/helpers.rs、comment.rs ×2、agent 工具、tiny_editor.rs → 在 Task 2/13 里退休）；§4.6（存量迁移）→ Task 5；§4.7（前端组件）→ Task 2/6–11/13；§4.8（样式）→ Task 12；§5（不做的事）→ 全文遵守。
- **顺序修正**：设计稿里 §4.3（存储）在 §4.7（前端组件）之后，但实现上必须**先切存储再切前端**（否则编辑器与存储格式对不上，中间态没法验证）。本计划把 Task 3/4/5 排在 Task 2 之前 —— Task 2 的 Interfaces 里已注明。
- **接口一致**：`richtext::sanitize_html` / `html_to_text` / `delta_to_html` / `detect_format` / `plain_to_html` 五个函数的签名在 Task 3/5 的 Interfaces 里定死，后续任务只引用不重复定义。`RichEditor` 的 props 与 `TinyEditor` 同构（只有 `initial` / `on_change` 的类型从 Delta 变成 HTML）。
- **Review Focus 覆盖**：第 1 条 → Task 3 Step 2 的白名单表 + Task 6–11 每次「保存后刷新」断言；第 2 条 → Task 5 Step 3 的「转换失败保留原样」；第 3 条 → Task 4 Step 1；第 4 条 → Task 4 Step 3；第 5 条 → Task 4 Step 1/Step 4 的兼容分支。
