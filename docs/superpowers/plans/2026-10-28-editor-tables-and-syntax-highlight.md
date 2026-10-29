# 编辑器表格与语法高亮（阶段 0）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在现有 `@opentiny/fluent-editor`（Quill 2.0 分支）上把「表格」与「语法高亮代码块」两个能力打开，让条目正文与评论编辑器具备这两项能力，不换编辑器、不改内容存储格式。

**Architecture:** 两项能力都已经随 vendor 的 bundle 提供了模块（`modules/better-table` 已经在主题默认里跑着，`modules/syntax` 已注册但缺 highlight.js）。本计划只做三件事：把 highlight.js vendor 进 `public/` 并保证它在 bundle 求值前就位；在 `glue.js` 的 `modules` 里打开 `syntax`；在工具栏里加 `better-table` 按钮。markdown 表格的识别与粘贴转换按既有的表格设计稿实现。内容仍存 Quill Delta，Rust 侧零改动。

**Tech Stack:** `@opentiny/fluent-editor` 3.25.12（vendored ESM）、`highlight.js` 11.12.0（BSD-3-Clause，vendor）、Leptos 0.8、cargo-leptos 0.3.7、playwright-core（headless Chromium）。

**Spec:** `docs/superpowers/specs/2026-10-28-editor-tiptap-migration-design.md`（§2.1 / §2.2 / §3）；表格部分另见 `docs/superpowers/specs/2026-10-28-editor-markdown-tables-design.md`（§3.1–§3.6）。

---

## Global Constraints

- **不新增 Rust 单元测试。** 通过门槛是 `make check`（native + wasm 两侧）+ `make build` + 浏览器真实交互验证。JS 侧同样不引入测试框架，用 `node --check` 做语法门。
- **编辑器静态资源 vendor 进 `public/`**：`Cargo.toml` 的 `assets-dir = "public"`，cargo-leptos 会把 `public/` 拷进 `target/site/`。运行时**不得依赖 CDN**（私有化部署是离线环境）。
- **构建不要抢用户的 `:3000`**：优先用用户自己的 `cargo leptos watch` 构建（免费）；只有在 watcher 不在时才自己跑 `LEPTOS_SITE_ROOT=target/site-smoke cargo leptos build`（私有 site root，绝不覆盖 `target/site`）。
- **一次只跑一个 cargo 构建**。两个并发构建会把 sccache 卡死（requests 3 / executed 0）。
- **每完成一个任务立即 commit**，前缀用 `feat` / `fix` / `chore` / `docs`。不 force-push，不带 `--no-verify`。
- **改完 CSS / 前端后要提醒用户「刷新一下」**：`cargo leptos watch` 的 live-reload websocket 跑在明文 :3001，而页面是 https，浏览器永远收不到热重载。
- **浏览器验证脚手架**用仓库里的 `scripts/editor-smoke.mjs`（Task 1 建），playwright-core 必须按绝对路径 default import（CommonJS，`NODE_PATH` 与具名 import 都不work）：
  `import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js'`
  Chromium 可执行文件：`~/Library/Caches/ms-playwright/chromium-1217/chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing`。
  **debug wasm 会被 `CompressionLayer` 卡住**（`Accept-Encoding: gzip` 下 >60s），必须用 `page.route('**/pkg/*.wasm')` 从磁盘 fulfill。

---

## Review Focus

以下五类是「规范暗示了、但没有任何任务的验证覆盖到」的失败面，按最容易咬人的顺序排：

1. **只读渲染路径的代码块高亮**：评论列表（`comment_list.rs:143`）与分享页（`share.rs:62`）走 `glue.js` 的 `toHtml()`。只读实例必须也挂 `syntax` 模块，且导出前要强制触发一次高亮 —— 否则评论里的代码块是纯文本。**导出结果里绝不能混进语言下拉的 `<select>`**。
2. **高亮的 debounce 会改模型**：syntax 模块每 1s 空闲后重新高亮并 `quill.update(USER)`，这会触发 `text-change` → Rust 侧 `on_change` → 乐观并发版本号作废 → 紧接着的「保存」被判成冲突。历史上 `on_uploaded` 已经因为同样的机制咬过一次（见 `tiny_editor.rs:20-25` 的注释）。
3. **Safari 拼音在代码块内输入**：syntax 模块把语言 `<select>` 作为 `uiNode` 挂进代码块，和当初吃掉列表项的 `<span class="ql-ui">` 是同一类结构（contenteditable=false 的兄弟节点）。代码块是**新路径**，没有历史修复覆盖。
4. **highlight.js 加载失败时编辑器必须优雅降级**：`Syntax` 模块构造函数在 `options.hljs == null` 时**抛异常**，抛在 `new FluentEditor(...)` 里会让整个编辑器不出现（正文区空白）。必须有兜底。
5. **`window.hljs` 的求值时序**：`fluent-editor.mjs` 在**模块求值时**就执行 `Ji.DEFAULTS = { hljs: window.hljs, ... }`。hljs 晚于它就位 → `DEFAULTS.hljs` 是 `undefined` → 构造函数抛 `Syntax module requires highlight.js`。

---

---

### Task 1: vendor highlight.js 并保证它在 bundle 之前就位

**Files:**
- Create: `public/tiny-editor/hljs.min.js`
- Modify: `src/app.rs`（`<Stylesheet id="tiny-editor">` / `<Script type_="module" src="/tiny-editor/glue.js">` 两行所在处，约 334–335 行）
- Create: `scripts/editor-smoke.mjs`

**Interfaces:**
- Consumes: 无（首个任务）
- Produces:
  - `public/tiny-editor/hljs.min.js` —— 经典脚本，顶层 `var hljs`，在浏览器里落到 `window.hljs`
  - `scripts/editor-smoke.mjs` —— 后续所有任务的浏览器验证入口，签名 `node scripts/editor-smoke.mjs <scenario>`，scenario 取值见后续各任务；退出码 0 = 通过，非 0 = 失败并在 stdout 打印失败原因

- [ ] **Step 1: 下载并 vendor highlight.js**

```bash
cd /Users/wangxiaoyan/Works/git/rodeo
curl -sS -o public/tiny-editor/hljs.min.js \
  https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.12.0/highlight.min.js
```

Expected: 文件约 129,254 字节；首行是 `/*!` 注释块，含 `Highlight.js v11.12.0` 与 `License: BSD-3-Clause`。

- [ ] **Step 2: 校验 vendor 的是 common 构建且语言覆盖 Quill 默认语言表**

```bash
cd /Users/wangxiaoyan/Works/git/rodeo
node -e "
const fs=require('fs'), vm=require('vm');
const ctx={console}; vm.createContext(ctx);
vm.runInContext(fs.readFileSync('public/tiny-editor/hljs.min.js','utf8'), ctx);
const need=['bash','cpp','csharp','css','diff','xml','java','javascript','markdown','php','python','ruby','sql'];
const missing=need.filter(l=>!ctx.hljs.getLanguage(l));
console.log('version', ctx.hljs.versionString, 'missing', missing);
if (missing.length) process.exit(1);
"
```

Expected: 打印 `version 11.12.0 missing []`，退出码 0。

- [ ] **Step 3: 在 shell 的 head 里放 hljs 的普通 script**

**放 shell 的 `<head>`，用普通 `<script src=...>` 元素 —— 不要放 App 视图，
更不要用 leptos_meta 的注册式 `<Script>` 组件。**

`src/app.rs` 的 `ShellInner` 组件里，`<head>` 内加一行（放在 `<HashedStylesheet>` 之前）：

```rust
<script src="/tiny-editor/hljs.min.js"></script>
```

**为什么不用 leptos_meta 的 `<Script>`**：注册式组件把自己收进 head 的 meta 缓冲，
客户端 hydrate 时由 `MetaContext` 的**一个共享游标**按注册顺序逐个消费 head 里的节点。
在 App 视图里多加一个注册式 `<Script>`，游标就跟 SSR 输出对不齐 → hydration 直接
panic，**而 SSR 出来的页面看着完全正常，只是前端不接管任何交互**：

```
A hydration error occurred ... leptos_meta-0.8.6/src/stylesheet.rs:38:14
The framework expected a marker node, but found this instead: <header class="appbar">
```

`page.on('pageerror')` 抓不到它（走的是 `console.error`）—— 只挂 pageerror 的验证脚本
会「全过」而页面实际是死的。shell 不参与 hydrate，普通元素就没有这套对齐问题。

**顺序仍然有保证**：普通 script 在解析期同步执行，module script 按规范延迟到文档解析完
才执行 —— 所以它一定早于 `glue.js` 里 `import FluentEditor from './fluent-editor.mjs'`
的求值（fluent-editor 在模块求值时就快照 `window.hljs`，晚于它就位的话 syntax 模块的
构造函数会抛 `Syntax module requires highlight.js`）。

- [ ] **Step 4: 类型检查**

Run: `make check`
Expected: native 与 wasm 两侧都通过，无新增 warning。

> 如果用户的 `cargo leptos watch` 活着，这一步的构建可以交给它（见 Global Constraints）；否则自己跑 `LEPTOS_SITE_ROOT=target/site-smoke cargo leptos build`。

- [ ] **Step 5: 建浏览器验证脚手架 `scripts/editor-smoke.mjs`**

写一个 node ESM 脚本，结构如下（后续任务只往 scenario 里加分支）：

```js
// 用法：node scripts/editor-smoke.mjs <scenario>
import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';
import fs from 'node:fs';
import path from 'node:path';

const EXECUTABLE = path.join(
  process.env.HOME,
  'Library/Caches/ms-playwright/chromium-1217/chrome-mac-x64',
  'Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
);

const scenario = process.argv[2];
if (!scenario) { console.error('usage: node scripts/editor-smoke.mjs <scenario>'); process.exit(2); }

// 从 target/debug/hash.txt 读出当前发布的三条哈希，拿到与胶水匹配的 wasm 文件名。
// hash-files 会留好几代 hashed 文件，按哈希名匹配，不要用 readdir 找任意一个。
function publishedWasm() {
  const hash = fs.readFileSync('target/debug/hash.txt', 'utf8')
    .split('\n').find(l => l.startsWith('wasm:'))?.split(/[:\s]+/)[1];
  if (!hash) throw new Error('target/debug/hash.txt 里没有 wasm 哈希');
  const p = `target/site/pkg/rodeo.${hash}.wasm`;
  if (!fs.existsSync(p)) throw new Error(`找不到 ${p}`);
  return p;
}

const browser = await pw.chromium.launch({ executablePath: EXECUTABLE, headless: true });
// …按 scenario 分支…
```

脚本必须做的事（每个 scenario 共用）：

1. `context.route('**/pkg/*.wasm', r => r.fulfill({ status: 200, contentType: 'application/wasm', body: fs.readFileSync(publishedWasm()) }))` —— 绕开 debug wasm 的 CompressionLayer 卡死。
2. `page.goto(url, { waitUntil: 'domcontentloaded', timeout: 120000 })`。
3. **不要预置 `localStorage.rodeo_theme`** —— 会造成 SSR 与客户端不一致的 hydration 报错。要切暗色就在加载完后 `document.documentElement.setAttribute('data-theme','dark')`。
4. 认证用 `context.addInitScript` 注入 `localStorage.rodeo_jwt`，值必须是 **JSON 编码后的字符串**（gloo_storage 会 JSON-encode），即 `JSON.stringify(JSON.stringify(token))`。
5. 退出前 `await browser.close()`。

scenario `hljs-loaded`：起一个 smoke server（见 Step 6），打开登录页，断言：

```js
const v = await page.evaluate(() => (typeof window.hljs));
// 期望 'object'
```

并断言页面 HTML 里 hljs 的 script 标签在 glue.js 之前：

```js
const order = await page.evaluate(() =>
  [...document.scripts].map(s => s.getAttribute('src')).filter(Boolean));
// 期望 order 中 '/tiny-editor/hljs.min.js' 的下标 < '/tiny-editor/glue.js' 的下标
```

- [ ] **Step 6: 起 smoke server 跑 scenario `hljs-loaded`**

```bash
cd /Users/wangxiaoyan/Works/git/rodeo
lsof -nP -iTCP:3099 -sTCP:LISTEN   # 先确认没有残留的孤儿进程占着 :3099
```

有残留就 `kill` 掉它（**只杀自己起的那个，别动用户的 :3000**）。然后：

```bash
cat > /tmp/rodeo-smoke.toml <<'EOF'
[server]
host = "127.0.0.1"
port = 3099

[storage]
data_dir = "/tmp/rodeo-smoke-data"

[storage.doc]
backend = "rocksdb"

[storage.blob]
backend = "local"

[search]
backend = "tantivy"
EOF
rm -rf /tmp/rodeo-smoke-data
LEPTOS_SITE_ADDR=127.0.0.1:3099 LEPTOS_SITE_ROOT=target/site \
  LEPTOS_HASH_FILES=true LEPTOS_OUTPUT_NAME=rodeo \
  ./target/debug/rodeo /tmp/rodeo-smoke.toml &
echo $! > /tmp/rodeo-smoke.pid
```

然后 `node scripts/editor-smoke.mjs hljs-loaded`，期望退出码 0。

**收尾：`kill $(cat /tmp/rodeo-smoke.pid)`，并 `rm -rf /tmp/rodeo-smoke-data`。留下孤儿进程是后续所有 UI 验证的假缺陷来源。**

- [ ] **Step 7: Commit**

```bash
git add public/tiny-editor/hljs.min.js src/app.rs scripts/editor-smoke.mjs
git commit -m "feat(editor): vendor highlight.js 并保证在 glue.js 之前加载"
```

---

### Task 2: 打开 syntax 模块（可编辑路径）

**Files:**
- Modify: `public/tiny-editor/glue.js`（`create()` 里的 `const modules = { ... }`，约 488–506 行）
- Modify: `public/tiny-editor/glue.js`（`RODEO_GLUE_VERSION`，约 12 行）

**Interfaces:**
- Consumes: Task 1 产出的 `public/tiny-editor/hljs.min.js`（页面加载后 `window.hljs` 已就位）
- Produces: `window.__rodeo_tiny_editor__.create(el, deltaJson, opts)` 行为不变，但编辑器实例现在带 syntax 模块

- [ ] **Step 0: 扩充语言下拉清单**

fluent-editor 的 `Hi.DEFAULTS.languages` 只有 14 项（plain/bash/cpp/cs/css/diff/
xml/java/javascript/markdown/php/python/ruby/sql），**没有 Rust/Go/TypeScript/
JSON/YAML**，而这些 highlight.js 全都支持。模块的 `languages` 选项可配，换成
开发者常用的完整清单：

```js
const SYNTAX_LANGUAGES = [
  { key: 'plain', label: '纯文本' }, { key: 'bash', label: 'Bash' },
  { key: 'shell', label: 'Shell' }, { key: 'c', label: 'C' },
  { key: 'cpp', label: 'C++' }, { key: 'cs', label: 'C#' },
  { key: 'css', label: 'CSS' }, { key: 'diff', label: '差异对比' },
  { key: 'go', label: 'Go' }, { key: 'java', label: 'Java' },
  { key: 'javascript', label: 'JavaScript' }, { key: 'json', label: 'JSON' },
  { key: 'kotlin', label: 'Kotlin' }, { key: 'markdown', label: 'Markdown' },
  { key: 'php', label: 'PHP' }, { key: 'python', label: 'Python' },
  { key: 'ruby', label: 'Ruby' }, { key: 'rust', label: 'Rust' },
  { key: 'sql', label: 'SQL' }, { key: 'swift', label: 'Swift' },
  { key: 'typescript', label: 'TypeScript' }, { key: 'xml', label: 'HTML/XML' },
  { key: 'yaml', label: 'YAML' },
];
```

**key 必须是 highlight.js 认识的名字或别名**（`cs` 是 `csharp` 的别名），
否则 `hljs.highlight(code, {language})` 抛 "Unknown language"。

- [ ] **Step 1: 在 `modules` 里加 syntax，并做降级兜底**

在 `glue.js` 的 `create()` 里，把

```js
const modules = {
  toolbar: [
    ['bold', 'italic', 'underline', 'strike'],
    [{ header: 1 }, { header: 2 }, { header: 3 }],
    [{ list: 'ordered' }, { list: 'bullet' }],
    ['blockquote', 'code-block'],
    ['link'],
    ['clean'],
  ],
};
```

改成

```js
// syntax 模块（Quill 的）：给 code-block 挂语言下拉 + highlight.js 高亮。
// 它在构造函数里检查 options.hljs == null 就抛异常 —— 抛在 new FluentEditor
// 里会让整个编辑器不出现（正文区空白），所以这里必须判空后再传。
const syntax = window.hljs ? { syntax: { hljs: window.hljs } } : {};
const modules = {
  ...syntax,
  toolbar: [
    ['bold', 'italic', 'underline', 'strike'],
    [{ header: 1 }, { header: 2 }, { header: 3 }],
    [{ list: 'ordered' }, { list: 'bullet' }],
    ['blockquote', 'code-block'],
    ['link'],
    ['clean'],
  ],
};
```

**注意**：`create()` 的 readonly 分支（`readOnly: true` 那个 `new FluentEditor`）也要同样处理 —— 那里也构造 FluentEditor，也需要 hljs 不为 null 才不抛。那里挂不挂 syntax 是 Task 3 的决定，但**兜底判空必须一起加**。

- [ ] **Step 2: 同步版本标记**

把 `glue.js` 里的 `const RODEO_GLUE_VERSION = '2026-10-22-ime-7';` 改成 `'2026-10-28-syntax-1'`。

诊断页会把它带进 trace，用来确认浏览器实际加载的是哪一版。

- [ ] **Step 3: 语法门**

```bash
cd /Users/wangxiaoyan/Works/git/rodeo
node --check public/tiny-editor/glue.js
```

Expected: 无输出，退出码 0。

- [ ] **Step 4: 浏览器验证 —— 语言下拉与高亮**

在 `scripts/editor-smoke.mjs` 里加 scenario `syntax-basic`：

1. 起 smoke server（同 Task 1 Step 6 的命令），用 GraphQL 登录拿 token（`mutation{ login(email:"admin@local", password:"Admin12345"){ token } }`），注入 `localStorage.rodeo_jwt`。
2. 建一个条目，用 `updateEntry`（或直接在编辑器里操作）写入内容：先在页面里点开条目详情，切到编辑态，在编辑器里输入三反引号包裹的 `fn main() { println!("hi"); }`，然后点工具栏的 code-block 按钮。
3. 断言：
   - 代码块内出现 `<select>`（语言下拉），且选项里含 `Rust` / `JavaScript` / `Plain`；
   - 下拉里选中 `rust` 后，代码块 DOM 里出现 `hljs` class 的 span（token）；
   - 控制台没有 `Syntax module requires highlight.js`。

- [ ] **Step 5: 浏览器验证 —— 高亮后内容仍能存能取**

scenario `syntax-persist`：

1. 在编辑器里写一个带语言的代码块并保存条目。
2. `page.reload()`，重新打开同一条目。
3. 断言代码块文本与保存前一致（**注意**：空 Quill 编辑器的 `innerText` 是 `"\n"` 不是 `""`，比较前先 `trim()`）。

- [ ] **Step 6: 回归 —— 既有能力没坏**

scenario `syntax-regression`：图片粘贴上传、@提及下拉、有序/无序列表、标题、链接各点一次，断言元素出现。**特别确认图片粘贴仍然上传成功**（`glue.js` 的 `attachImageUpload` 与 syntax 模块同时挂在容器上，监听顺序不能互相 `preventDefault` 掉）。

- [ ] **Step 7: Commit**

```bash
git add public/tiny-editor/glue.js scripts/editor-smoke.mjs
git commit -m "feat(editor): 打开 syntax 模块，代码块带语言下拉与语法高亮"
```

---

### Task 3: 只读渲染路径也要高亮，且导出里不能混进 `<select>`

**Files:**
- Modify: `public/tiny-editor/glue.js`（`toHtml()` 里的共享渲染实例，约 540–560 行）

**Interfaces:**
- Consumes: Task 2 的 `modules.syntax` 写法
- Produces: `window.__rodeo_tiny_editor__.toHtml(deltaJson) -> string` —— 返回的 HTML 里代码块**带 token 高亮**且**不含 `<select>`**

- [ ] **Step 1: 渲染实例挂 syntax，导出前强制高亮，导出后剥掉 select**

`toHtml()` 里那个共享渲染实例目前是：

```js
renderEditor = new FluentEditor(renderHost, {
  readOnly: true,
  modules: { toolbar: false },
});
```

改成：

```js
renderEditor = new FluentEditor(renderHost, {
  readOnly: true,
  modules: {
    toolbar: false,
    // 只读实例也要挂 syntax，否则评论里的代码块导出出来是纯文本。
    ...(window.hljs ? { syntax: { hljs: window.hljs } } : {}),
  },
});
```

**`create()` 的 readonly 分支（分享页的条目正文）同样要挂 syntax** —— 它也是一个独立的 FluentEditor 实例，不挂就没有高亮。而它挂上之后，语言下拉（`<select>`，syntax 模块的 uiNode）会作为一个可交互控件出现在只读页面里，所以：

- readonly 分支给容器加 `.ql-readonly` 类（**在 JS 里加，不在 Rust 的 `view!` 里写 class** —— Leptos 重渲染会把 class 整体重写成模板里的值）；
- `style/main.css` 加 `.ql-readonly select { display: none }` 把它藏掉。

然后在 `renderEditor.setContents(parsed, 'silent')` 之后、`getSemanticHTML()` 之前，插入强制高亮：

```js
// syntax 模块的高亮是 debounce 1s 的（SCROLL_OPTIMIZE 触发），
// 刚 setContents 就导出会拿到没高亮的纯文本。这里手动同步跑一次。
const syntax = renderEditor.getModule('syntax');
if (syntax && typeof syntax.highlight === 'function') {
  try { syntax.highlight(); } catch (e) { /* 高亮失败不影响导出 */ }
}
```

导出后再兜一层，把语言下拉从结果里剥掉（只读视图里不该有可交互控件）：

```js
let html = renderEditor.getSemanticHTML();
// 语言下拉是 code-block 的 uiNode，正常不进 getSemanticHTML 的输出；
// 万一进去了也不能漏到只读视图里。
html = html.replace(/<select\b[\s\S]*?<\/select>/gi, '');
```

**顺带修一个现存缺陷**：`toHtml` 的兜底条件要从「JSON.parse 失败」改成「**解析不出 Delta 形状**」—— 老正文可能恰好是 `123`、`"foo"`、`{"a":1}`，`JSON.parse` 成功但不是 Delta 形状，`setContents` 抛错，还是落进 catch → 空白。老评论存的是纯文本，现在在只读视图里是**空白**的。改成：parse 失败 **或** 结果不是 `{ops:[…]}` / `[…]` 形状 → 一律按纯文本兜底（`{ops:[{insert: 原文}]}`）。

- [ ] **Step 2: 语法门**

```bash
cd /Users/wangxiaoyan/Works/git/rodeo
node --check public/tiny-editor/glue.js
```

Expected: 无输出，退出码 0。

- [ ] **Step 3: 浏览器验证 —— 评论列表里的代码块有高亮**

scenario `syntax-readonly`：

1. 用 GraphQL 建一条评论，body 是带 `code-block` 的 Delta JSON（形如
   `{"ops":[{"insert":"fn main() {}\n","attributes":{"code-block":true}},{"insert":"\n"}]}`）。
2. 打开条目详情页，断言评论正文里：
   - 有 `pre` / `code` 元素；
   - **不含** `<select>`；
   - 该评论是只读渲染，**没有**工具栏。

- [ ] **Step 4: 浏览器验证 —— 分享页同样**

scenario `syntax-share`：打开匿名分享页，断言同上（分享页正文走 `TinyEditor readonly`，评论走 `share.rs:62` 的 `delta_to_html`）。

- [ ] **Step 5: Commit**

```bash
git add public/tiny-editor/glue.js scripts/editor-smoke.mjs
git commit -m "feat(editor): 只读渲染路径也高亮代码块，导出剥掉语言下拉"
```

---

### Task 4: 工具栏加表格按钮

**Files:**
- Modify: `public/tiny-editor/glue.js`（`create()` 的 `modules.toolbar` 数组，约 489–496 行；以及 `toHtml()` 的渲染实例，约 540 行）
- Modify: `style/main.css`（若需要补表格样式；`public/tiny-editor/style.css` 里已有 `quill-better-table` 的 15 处规则，大概率不用动）

**Interfaces:**
- Consumes: 无新接口
- Produces: 工具栏出现表格按钮；`toHtml()` 的渲染实例补 `'better-table': true`

- [ ] **Step 1: 工具栏加一行**

在 `modules.toolbar` 数组里，`['link'],` 之后、`['clean'],` 之前插入：

```js
['better-table'],
```

**不要写 `'better-table': true`**：`themes/snow` 的 `DEFAULTS.modules` 已经带了 `"better-table": {operationMenu:{color:true}}`，显式写 `true` 会用 `{}` 覆盖掉主题的右键菜单配置，白白丢掉「背景色」菜单。Quill 的 `resolveOptions` 对 `modules` 做深合并，数组形式的工具栏会被包成 `{container:[...]}` 再与主题的 `{handlers:{...}}` 深合并，主题自带的 `insertTable(3,3)` handler 不会丢。

- [ ] **Step 2: `toHtml()` 的渲染实例补 better-table**

共享渲染实例用的是 default theme，没有 better-table 模块，而 `table-view` blot 的 scroll 处理器会 `getModule("better-table")`，缺模块就 TypeError。改成：

```js
renderEditor = new FluentEditor(renderHost, {
  readOnly: true,
  modules: {
    toolbar: false,
    // 渲染实例用 default theme，没有 better-table；table-view blot 的
    // scroll 处理器 getModule('better-table') 缺模块会 TypeError。
    'better-table': true,
    ...(window.hljs ? { syntax: { hljs: window.hljs } } : {}),
  },
});
```

- [ ] **Step 3: 语法门 + 类型检查**

```bash
cd /Users/wangxiaoyan/Works/git/rodeo
node --check public/tiny-editor/glue.js
make check
```

Expected: 均通过。

- [ ] **Step 4: 浏览器验证 —— 插表与表格能力**

> **实测发现**：better-table 的右键菜单文案是**硬编码英文**（`Merge Cells` /
> `Split Cells` / `Delete Entire Row` / `Insert Rows Above` …），fluent-editor
> 没给它做 i18n，而 rodeo 的界面是中文的。断言要按英文写。菜单文案本身不可配
> （模块没暴露 label 选项），要中文化得改 vendor bundle —— 本次不做，记在
> 闸口结论里。

scenario `table-basic`：

1. 打开条目详情的编辑器，断言工具栏出现 `ql-better-table` 按钮。
2. hover 该按钮 → 断言出现行列选择器（better-table 包的 hover 选择器）。
3. 点按钮 → 断言编辑器 DOM 里出现 3×3 表格（`table` 元素 + 9 个 `td`）。
4. 在第一个单元格里打字 → 断言文字进去了。
5. 右键单元格 → 断言菜单里有「合并单元格」「背景色」「删除行/列」。
6. 保存条目 → `page.reload()` → 断言表格还在、单元格内容还在。

- [ ] **Step 5: 浏览器验证 —— 只读渲染出表格**

scenario `table-readonly`：把上面那条带表格的评论渲染出来，断言只读视图里有 `<table>` 且结构正确。

- [ ] **Step 6: Commit**

```bash
git add public/tiny-editor/glue.js
git commit -m "feat(editor): 工具栏加表格按钮，只读渲染实例补 better-table 模块"
```

---

### Task 5: markdown 表格的识别、粘贴转换与只读渲染

**Files:**
- Modify: `public/tiny-editor/glue.js`

**Interfaces:**
- Consumes: Task 4 的表格能力
- Produces: `glue.js` 内部的一组新函数（不导出到全局）：

```js
function escapeHtml(s)                       // HTML 转义
function splitMdRow(line)                    // 一行 → 单元格数组
function isMdSeparator(cells)                // 分隔行判定
function parseMdTableAt(lines, i)            // 从第 i 行试解析一张表 → {header, rows, start, end} | null
function scanMdTables(lines)                 // 扫出文本里的所有表
function mdTableHtml(table)                  // 表 → <table> DOM
function textWithMdTables(text)              // 纯文本 → HTML（表格段换 <table>）
function htmlWithMdTables(html)              // HTML → HTML（markdown 表块换 <table>）
function attachMarkdownTablePaste(el, editor)  // capture 阶段的 paste 监听
function splitOpsByMdTables(ops, editor)     // 只读路径：Delta op → 表格 ops
```

- [ ] **Step 1: 实现表格识别**

规则（与 `2026-10-28-editor-markdown-tables-design.md` §3.2 一致，**必须与 Rust 侧 `agent_panel.rs` 的实现逐条对齐**）：

- 表头行：含 `|` 的行，按 `|` 切分（`\|` 是字面竖线），首尾 trim，**行内代码 span 里的 `|` 不算分隔符**。
- 分隔行：每格匹配 `^:?-+:?$`，且格数与表头行相同。
- 表头行**至少 2 格**才认（压掉「a | b」这种普通文本被误判）。
- 数据行：紧跟分隔行之后的连续「含 `|` 的行」。
- 格数不一致时按最大列数补空。
- 分隔行的 `:` 对齐标记**不实现**。

- [ ] **Step 2: 表格 → HTML，再用 clipboard matcher 转成原生表格**

**不要手搓 better-table 的 Delta ops。** better-table 模块的 `insertTable(rows, cols)` 产出的 Delta 形状很琐碎（`table-col` 行 + 每格一个 `table-cell-line` + 随机 row/cell id），手搓必然对不上。

改成走模块自己的路：

1. `mdTableHtml(table)` 生成 `<table><tbody><tr><td>…` 的 DOM（表头行用行内 `<strong>` 加粗，不用模块自带的「表头单元格」格式 —— 那个在只读导出时会变成 `<h1>`，套在 `<td>` 里很难看）。
2. `editor.clipboard.convert({ html, text: '' })` —— Quill 公开的同步方法，走一遍 clipboard matcher，better-table 自带的 td/tr matcher 在这条路径上，表格因此是「模块自己认的」原生表格。

**实现后必须在浏览器里对比**「工具栏插出来的表格」与「粘贴转换出来的表格」两者的 `getContents()` 与 DOM 结构，不一致就以模块为准调整。

- [ ] **Step 3: 语法门**

```bash
cd /Users/wangxiaoyan/Works/git/rodeo
node --check public/tiny-editor/glue.js
```

- [ ] **Step 4: 浏览器验证 —— 粘贴转换**

scenario `table-paste`，四个子场景：

1. 纯文本 markdown 表格（`| a | b |\n| --- | --- |\n| 1 | 2 |`）→ 变成真表格。
2. 带粗体正文的 HTML 剪贴板（`<p><strong>正文</strong></p><p>| a | b |…</p>`）→ 正文仍是粗体，表格是真表格。
3. HTML 原生 `<table>` → 走 Quill 默认路径（better-table 自带的 td/tr clipboard matcher），不炸。
4. **往表格单元格里粘贴普通文本** → 粘进单元格，**不插新表**。

第 4 条的放行条件（设计稿 §3.4）：光标不在表格里才接管 —— 判定用 `editor.getFormat(sel.index)` 里没有 `table-cell-line` / `table-col`；且剪贴板里没有图片文件（有则交给现有的图片上传守卫，否则两个 capture 监听都会 `preventDefault`）。

- [ ] **Step 5: 浏览器验证 —— 只读渲染老内容里的 markdown 表格**

scenario `table-readonly-legacy`：建一条纯文本评论，内容含 markdown 表格。断言只读视图渲染出 `<table>`，且**正文不再空白**（今天的老评论在只读视图里是空白的 —— `toHtml` 解析不出 Delta 就 `catch` 返回空串）。

兜底条件要写成「解析不出 Delta」而不只是「JSON.parse 失败」：老正文可能恰好是 `123`、`"foo"`、`{"a":1}` —— `JSON.parse` 成功但不是 Delta 形状，`setContents` 会抛错，还是落进 catch → 空白。

- [ ] **Step 6: Commit**

```bash
git add public/tiny-editor/glue.js scripts/editor-smoke.mjs
git commit -m "feat(editor): 粘贴 markdown 表格自动转成编辑器原生表格"
```

---

### Task 6: AI 对话面板渲染 markdown 表格

**Files:**
- Modify: `src/frontend/agent_panel.rs`（`render_markdown` / `render_text_segment`）
- Modify: `style/main.css`（新增 `.agent-msg table.md-table` 规则）

**Interfaces:**
- Consumes: Task 5 的 `parseMarkdownTable` 规则（**Rust 侧独立实现一份，规则必须逐条一致**）
- Produces: `agent_panel.rs` 内部新增

```rust
// 判定一段文本是不是 markdown 表格；是则返回 (表头行, 数据行)
fn parse_markdown_table(text: &str) -> Option<(Vec<String>, Vec<Vec<String>>)>
```

- [ ] **Step 1: 实现 Rust 侧的 `parse_markdown_table`**

规则与 JS 侧逐条一致：表头行至少 2 格、分隔行每格匹配 `^:?-+:?$` 且格数相同、`\|` 转义、行内代码 span 里的 `|` 不算分隔符、格数不一致按最大列数补空、不实现对齐标记。

**两边规则必须一致**（同一张表在编辑器里是一张表、在对话面板里是竖线）。改动时两边一起改；规则本身很小（各 ~40 行），不值得为它引入跨语言共享。

- [ ] **Step 2: `render_markdown` 支持表格**

按设计稿 §3.6：

1. 现有 ``` 围栏切分逻辑不动 —— 围栏内的内容不当表格处理。
2. 文本段先按行扫识别表格，把段落切成「正文段 / 表格段」交替。
3. 表格段渲染成：

   ```html
   <table class="md-table">
     <thead><tr><th>…</th></tr></thead>
     <tbody><tr><td>…</td></tr></tbody>
   </table>
   ```

   表头行进 `<thead>`，数据行进 `<tbody>`；首行若没有数据行也要出 `<tbody>`（空）。
4. 单元格内容做 markdown 行内渲染（`**粗体**` / `` `code` `` / `[链接](url)`）—— 与现有 `render_inline` 的约定一致（输入必须已 escape，先 `html_escape` 再走行内替换）。
5. 正文段沿用现有逻辑（escape → 行内替换 → `\n` → `<br>`），**但表格前的正文段末尾那个 `<br>` 去掉**，否则表格上方会多一行空白。

- [ ] **Step 3: 加样式**

在 `style/main.css` 里加一组 `.agent-msg table.md-table` 的规则：边框、表头底色、单元格 padding、横向滚动。

**不要复用现有的 `.tbl`** —— 那是视图表格。

- [ ] **Step 4: 类型检查**

Run: `make check`
Expected: native 与 wasm 两侧通过。

- [ ] **Step 5: 浏览器验证**

scenario `agent-md-table`：在 AI 对话面板里让模型输出一张 markdown 表格，断言渲染出 `<table class="md-table">`、表头行进 `<thead>`、数据行进 `<tbody>`，且样式不撑破聊天区。

- [ ] **Step 6: Commit**

```bash
git add src/frontend/agent_panel.rs style/main.css
git commit -m "feat(agent): 对话面板把模型输出的 markdown 表格渲染成 table"
```

---

### Task 7: 阶段 0 闸口评估

**Files:**
- Create: `docs/superpowers/specs/2026-10-28-editor-phase0-verdict.md`

**Interfaces:**
- Consumes: Task 1–6 的全部产出
- Produces: 一份评估结论文档，决定是否进入阶段 1（Tiptap 迁移）

- [ ] **Step 1: 跑完整验证清单**

按设计稿 §7「阶段 0」的 10 条，逐条用 `scripts/editor-smoke.mjs` 的 scenario 跑一遍并记录结果：

1. 工具栏出现表格按钮；hover 出行列选择器；点一下插出 3×3 表格；单元格内能打字、能换行。
2. 右键菜单有「合并单元格 / 背景色 / 删行列」。
3. 表格内容保存后刷新仍在。
4. 粘贴 markdown 表格 → 变成真表格；粘贴「带粗体的正文 + markdown 表格」→ 正文仍是粗体。
5. 往表格单元格里粘贴普通文本 → 粘进单元格，不插新表。
6. 代码块按钮 → 代码块内出现语言下拉；选 Rust/JS → 高亮生效；内容保存后刷新仍在。
7. 编辑代码块后静置 1s，观察是否产生多余的状态变更 / 保存请求（**对应 Review Focus 第 2 条**）。
8. 回归：图片粘贴上传、@提及、勾选列表、有序/无序列表、标题、链接。
9. 中文输入法（Safari 拼音）在列表项 / 表格单元格 / 代码块内输入都不丢字（**对应 Review Focus 第 3 条**；Safari 走人工验证，headless Chromium 不覆盖 IME）。
10. AI 对话面板：模型输出 markdown 表格 → 渲染成 `<table>`。

- [ ] **Step 2: 写评估结论**

`docs/superpowers/specs/2026-10-28-editor-phase0-verdict.md` 里逐条记：通过 / 失败 / 未能验证（并写清为什么未能验证），然后按设计稿 §3.5 的判据给出结论：

- **取消迁移** —— 全部通过 → 后续只做维护，阶段 1 的计划作废。
- **进入阶段 1** —— 出现表格缺列宽拖拽 / 代码块高亮在 Safari 出问题 / 补丁层继续失控 / 需要编辑器级可扩展性 中的任一项。

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-10-28-editor-phase0-verdict.md
git commit -m "docs(editor): 阶段 0 闸口评估结论"
```

---

## 自检

- **规范覆盖**：设计稿 §3.1（工具栏）→ Task 4；§3.2（hljs 落地）→ Task 1；§3.3（syntax 模块）→ Task 2；§3.4（只读渲染）→ Task 3；表格设计稿 §3.1–§3.5 → Task 4–5；§3.6（AI 面板）→ Task 6；§3.5（闸口）→ Task 7。设计稿 §4（阶段 1 / Tiptap）**不在本计划内**，见 `2026-10-28-editor-tiptap-migration.md`。
- **接口一致**：`parseMarkdownTable` / `buildTableOps` / `splitOpsByMarkdownTable` 三个 JS 函数与 Rust 侧 `parse_markdown_table` 的规则一致，命名在 Task 5 / Task 6 的 Interfaces 里定死，两处都引用。
- **Review Focus 覆盖**：第 1 条 → Task 3 Step 3–4；第 2 条 → Task 7 Step 1 第 7 项；第 3 条 → Task 7 Step 1 第 9 项；第 4 条 → Task 2 Step 1 的判空兜底；第 5 条 → Task 1 Step 3 + Step 5。
