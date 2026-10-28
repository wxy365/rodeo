// Rodeo tiny-editor 桥接脚本（ES module）。
// 由 app shell 以 <script type="module" src="/tiny-editor/glue.js"> 加载。
// 暴露 window.__rodeo_tiny_editor__.create(el, deltaJson) 供 Rust/WASM 侧调用。
//
// 升级方式：npm 包名实为 @opentiny/fluent-editor（Quill 2.0 版）。
//   - fluent-editor.mjs ← https://esm.sh/@opentiny/fluent-editor@<ver>/es2022/fluent-editor.bundle.mjs
//   - style.css         ← https://unpkg.com/@opentiny/fluent-editor@<ver>/style.css
//
// hljs.min.js 是 highlight.js 11.12.0 的官方浏览器构建（BSD-3-Clause），
// 从 cdnjs 抓的一份 UMD 产物，直接 commit 进仓库（和 fluent-editor.mjs 一样的做法）：
//   - hljs.min.js       ← https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.12.0/highlight.min.js
// 它必须在 glue.js 之前加载：fluent-editor.mjs 在模块求值时就快照 window.hljs，
// 晚于它就位的话 syntax 模块的构造函数会抛 "Syntax module requires highlight.js"。
// app.rs 里用的是普通 <script>（非 module），HTML 规范保证它先于 module script 执行。
import FluentEditor from './fluent-editor.mjs';

// 版本标记：诊断页会把它带进 trace，用来确认浏览器实际加载的是哪一版。
// 每次修改本文件时同步更新。
const RODEO_GLUE_VERSION = '2026-10-28-syntax-1';
window.__rodeo_glue_version__ = RODEO_GLUE_VERSION;

// syntax 模块的语言下拉清单。fluent-editor 默认只给 14 项（没有 Rust/Go/
// TypeScript/JSON/YAML），而这些 highlight.js 全都支持 —— 模块的 languages
// 选项是可配的，这里换成开发者常用的完整清单。
//
// key 必须是 highlight.js 认识的语言名或别名（'cs' 是 csharp 的别名，
// 'shell'/'bash' 都是独立条目），否则 hljs.highlight 会抛 "Unknown language"。
const SYNTAX_LANGUAGES = [
  { key: 'plain', label: '纯文本' },
  { key: 'bash', label: 'Bash' },
  { key: 'shell', label: 'Shell' },
  { key: 'c', label: 'C' },
  { key: 'cpp', label: 'C++' },
  { key: 'cs', label: 'C#' },
  { key: 'css', label: 'CSS' },
  { key: 'diff', label: '差异对比' },
  { key: 'go', label: 'Go' },
  { key: 'java', label: 'Java' },
  { key: 'javascript', label: 'JavaScript' },
  { key: 'json', label: 'JSON' },
  { key: 'kotlin', label: 'Kotlin' },
  { key: 'markdown', label: 'Markdown' },
  { key: 'php', label: 'PHP' },
  { key: 'python', label: 'Python' },
  { key: 'ruby', label: 'Ruby' },
  { key: 'rust', label: 'Rust' },
  { key: 'sql', label: 'SQL' },
  { key: 'swift', label: 'Swift' },
  { key: 'typescript', label: 'TypeScript' },
  { key: 'xml', label: 'HTML/XML' },
  { key: 'yaml', label: 'YAML' },
];

// 三个编辑器实例（可编辑分支 / 只读分支 / toHtml 渲染实例）都要挂同一个
// syntax 配置。window.hljs 判空是必须的：syntax 模块的构造函数在
// options.hljs == null 时会抛异常，抛在 new FluentEditor 里会让整个编辑器挂不上。
function syntaxModule() {
  if (!window.hljs) return {};
  return { syntax: { hljs: window.hljs, languages: SYNTAX_LANGUAGES } };
}

// ── 列表 Delta 兼容层 ────────────────────────────────────────────────
// fluent-editor 3.x 自带的 "formats/list" blot（内部类，注册在
// FluentEditor.imports）跟 Quill 标准的 List 不一样：
//   - DOM 用 `<ol class="ordered"><li class="ordered">`，不是 data-list；
//   - `formats()` 返回 `{value: "ordered"}` 对象，Delta 里就写成
//     `{"list":{"value":"ordered"}}`。
// 而它的 `getSemanticHTML()`（从 Quill 搬来的 HTML 导出器）只认字符串
// `{"list":"ordered"}`：
//   - `kl()` 里 `r === "ordered"` 比较不上 → 有序列表被导成 <ul>；
//   - `Vr()` 里相邻项的 `l === d` 比较不上（每次 formats() 都新建对象）
//     → 每个 <li> 各包一层 <ul>/<ol>，有序列表每项都从 1 重新编号。
// 这里两件事：
//   1) `normalizeListAttributes` 把对象形式压回字符串，读写两侧统一成
//      Quill 标准形状（te.create / te.format 都接受字符串）；
//   2) 导出期间让静态 formats() 返回字符串，导出器才能按类型分组。
// 只在导出窗口内改返回值，编辑器自身的 optimize/checkbox 逻辑仍拿到对象。
const ListBlot = FluentEditor.imports && FluentEditor.imports['formats/list'];
const origListFormats = ListBlot && ListBlot.formats;
let exportingSemantic = false;
if (origListFormats) {
  ListBlot.formats = function (domNode, scroll) {
    const v = origListFormats.call(this, domNode, scroll);
    if (exportingSemantic && v && typeof v === 'object') {
      return v.value;
    }
    return v;
  };
}

// ── 列表标记节点（Safari 输入法）────────────────────────────────────
// fluent-editor 的列表 blot 会在每个 <li> 里塞一个
// `<span class="ql-ui" contenteditable="false">` 作为第一个子节点。
// 有序/无序列表的标记其实由 CSS `li.ordered::before` / `li.bullet::before`
// 绘制，这个 span 是纯装饰、还破坏 Safari 的输入法提交：
//
//   Safari 提交中文时先 `deleteCompositionText`（删掉拼音），
//   再 `insertFromComposition`（插入汉字）。删完拼音的瞬间，LI 里只剩
//   这个 contenteditable=false 的 span——WebKit 认为该块「没有可编辑
//   内容」，会把整个 LI 移除并换成 <br> 占位；随后的汉字被插到 <ol> 上，
//   Quill 收不进列表，列表和文字一起消失。
//   （2026-10-22 用 Safari 26.2 + 系统拼音抓到的 DOM trace 证实。）
//
// 所以有序/无序列表不再挂 uiNode；勾选列表（checked/unchecked）需要
// 可点击的方框，保持原行为。
const origAttachUI = ListBlot && ListBlot.prototype && ListBlot.prototype.attachUI;
if (origAttachUI) {
  ListBlot.prototype.attachUI = function (node) {
    let value;
    try {
      const f = this.statics.formats(this.domNode, this.scroll);
      value = f && typeof f === 'object' ? f.value : f;
    } catch (e) {
      value = undefined;
    }
    if (value === 'ordered' || value === 'bullet') {
      this.uiNode = null;
      // 复刻 attachUI 里「空块补默认子节点」的行为，只是不插入 uiNode。
      if (this.children.length === 0 && this.statics.defaultChild != null) {
        const child = this.scroll.create(this.statics.defaultChild.blotName);
        this.appendChild(child);
      }
      this.cache = {};
      return;
    }
    return origAttachUI.call(this, node);
  };
}

// 把 `{"list":{"value":"ordered"}}` 压成 `{"list":"ordered"}`（原地修改并返回）。
// 兼容两种输入：字符串（标准 Quill / 修复后的数据）与对象（历史数据）。
function normalizeListAttributes(delta) {
  if (!delta || !Array.isArray(delta.ops)) return delta;
  for (const op of delta.ops) {
    const list = op.attributes && op.attributes.list;
    if (list && typeof list === 'object' && typeof list.value === 'string') {
      op.attributes.list = list.value;
    }
  }
  return delta;
}

// 附属节点（工具栏/浮层）→ 宿主编辑器容器。
// 只按容器清理是不够的：Leptos 重挂载会换一个新容器，旧容器上的工具栏就成了
// 孤儿——它留在仍然存活的父节点里层层堆叠，并挡住下面的按钮（评论「保存」按钮
// 被残留工具栏盖住过）。用这张表记下每个附属节点属于谁，就能认出孤儿。
const auxOwners = new Map();

// 清掉宿主已离开文档的附属节点。当前页面上活着的编辑器不受影响。
function sweepOrphans() {
  for (const [node, host] of auxOwners) {
    if (!host.isConnected) {
      node.remove();
      auxOwners.delete(node);
    }
  }
}

// 只清理本容器上一次留下的工具栏/浮层。Quill 的 snow 主题把工具栏插成容器的
// 前一个兄弟节点，容器卸载时不会被一并带走；早先这里按类名全局清理，但页面上
// 同时存在详情编辑器与评论编辑器之后，全局清理会抹掉另一个编辑器的工具栏。
function cleanupAux(el) {
  if (el.__rodeo_aux) {
    el.__rodeo_aux.forEach((n) => {
      auxOwners.delete(n);
      n.remove();
    });
    el.__rodeo_aux = [];
  }
  closeStrayTooltips();
}

// 强清 body 上残留下来的 ql-tooltip 浮层（含 .ql-editing 状态）：
// 之前只 remove()，但如果浮层在 mount 之后被父级 re-render「钉死」保留
// `.ql-editing`，input[type=text] 仍保持 display:inline-block 可见。
// 先撤销 editing 态再 remove，对 link tooltip 类 浮层通用。
// 暴露成 `window.__rodeo_close_tooltips__` 给 Rust 侧在
// `LabelEditor.on_changed` 等「数据更新、可能让父组件重渲染」
// 的钩子里调用，把组件重挂清不到的浮层一并关掉。
function closeStrayTooltips() {
  document.querySelectorAll('body > .ql-tooltip').forEach((n) => {
    n.classList.remove('ql-editing');
    n.remove();
  });
}
window.__rodeo_close_tooltips__ = closeStrayTooltips;

// 正文支持图片：不再是「拦截」，而是接手上传。粘贴/拖入的图片文件经 opts.upload
// 上传后以 /api/attachments/<id> 的 URL 插入，而不是 Quill 默认的 base64 data URI
// （后者会让单条内容涨到数 MB，且只读渲染会把 data: 原样输出）。
// 仍在捕获阶段监听：Quill 的 clipboard 模块在冒泡阶段处理粘贴，要抢在它之前。
function attachImageUpload(el, editor, opts) {
  const imageFiles = (dt) => {
    const items = dt && dt.items;
    if (!items) return [];
    return Array.from(items)
      .filter((it) => it.kind === 'file' && (it.type || '').startsWith('image/'))
      .map((it) => it.getAsFile())
      .filter(Boolean);
  };
  const guard = (e) => {
    const files = imageFiles(e.clipboardData || e.dataTransfer);
    if (!files.length) return;
    e.preventDefault();
    e.stopPropagation();
    if (!opts || typeof opts.upload !== 'function') return;
    uploadIntoEditor(editor, opts.upload, files);
  };
  el.addEventListener('paste', guard, true);
  el.addEventListener('drop', guard, true);
}

// 串行插入：每个文件上传完成后再动下一个，位置由前一个的结果决定，
// 避免并发 await 让插入点互相错位。占位文本会在上传完成后被替换掉，
// 且保存的是 getContents() 的结果，占位不会落库。
async function uploadIntoEditor(editor, upload, files) {
  const sel = editor.getSelection(true);
  let index = sel ? sel.index : editor.getLength();
  for (const file of files) {
    const at = index;
    const placeholder = '上传中…';
    editor.insertText(at, placeholder, 'user');
    index = at + placeholder.length;
    try {
      const url = await upload(file);
      editor.deleteText(at, placeholder.length, 'user');
      editor.insertEmbed(at, 'image', url, 'user');
      index = at + 1;
    } catch (err) {
      editor.deleteText(at, placeholder.length, 'user');
      const fail = '图片上传失败';
      editor.insertText(at, fail, 'user');
      index = at + fail.length;
      console.warn('图片上传失败', err);
    }
    editor.setSelection(index, 0);
  }
}

// ── markdown 表格 ────────────────────────────────────────────────────
// 识别规则（Rust 侧 src/frontend/agent_panel.rs 有一份等价实现，两边必须逐条一致，
// 否则同一张表在编辑器里是表、在对话面板里是竖线）：
//   表格   := 表头行 分隔行 (数据行)*
//   表头行 := 含 '|' 的行，切出 N 个单元格，N >= 2
//   分隔行 := 每格匹配 /^:?-+:?$/，且格数与表头行相同
//   数据行 := 紧跟分隔行之后的连续「含 '|' 的行」
// 切格：按 '|' 切（'\|' 是字面竖线），首尾 trim；行内代码 span 里的 '|' 不算分隔符。
// 分隔行的 ':' 对齐标记不实现；各行格数不一致时按最大列数补空。
// 表头至少 2 格 —— 用来压掉「a | b」这种普通文本被误判成表格。

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

// 把一行切成单元格。行内代码 span 与转义竖线先换成占位符，切完再还原。
function splitMdRow(line) {
  const codes = [];
  let t = line.trim();
  t = t.replace(/`[^`]*`/g, (m) => { codes.push(m); return `\u0000${codes.length - 1}\u0000`; });
  t = t.replace(/\\\|/g, '\u0001');
  const parts = t.split('|');
  // 行首/行尾的竖线只是 markdown 装饰，切完要去掉对应的空串
  if (t.startsWith('|')) parts.shift();
  if (t.endsWith('|') && parts.length) parts.pop();
  return parts.map((p) => p.trim()
    .replace(/\u0001/g, '|')
    .replace(/\u0000(\d+)\u0000/g, (_, i) => codes[Number(i)]));
}

function isMdSeparator(cells) {
  return cells.length > 0 && cells.every((c) => /^:?-+:?$/.test(c));
}

// 从第 i 行开始试解析一张表。成功返回 { header, rows, start, end }，
// end 是数据行之后的行号（exclusive）；失败返回 null。
function parseMdTableAt(lines, i) {
  if (i + 1 >= lines.length) return null;
  const headerLine = lines[i];
  if (!headerLine.includes('|')) return null;
  const header = splitMdRow(headerLine);
  if (header.length < 2) return null;
  const sep = splitMdRow(lines[i + 1]);
  if (sep.length !== header.length) return null;
  if (!isMdSeparator(sep)) return null;
  const rows = [];
  for (let j = i + 2; j < lines.length; j++) {
    if (!lines[j].includes('|')) break;
    rows.push(splitMdRow(lines[j]));
  }
  const cols = rows.reduce((m, r) => Math.max(m, r.length), header.length);
  while (header.length < cols) header.push('');
  for (const r of rows) { while (r.length < cols) r.push(''); }
  return { header, rows, start: i, end: i + 2 + rows.length };
}

// 扫出一段文本里的所有 markdown 表格（互不重叠）。
function scanMdTables(lines) {
  const out = [];
  let i = 0;
  while (i < lines.length) {
    const t = parseMdTableAt(lines, i);
    if (t) { out.push(t); i = t.end; }
    else i++;
  }
  return out;
}

// 表格 → HTML。表头行整行加粗（用行内 <strong>，不用模块自带的「表头单元格」
// 格式 —— 那个在只读导出时会变成 <h1>，套在 <td> 里很难看）。
// 单元格内容按字面文本处理（编辑器路径不解析 markdown 标记）。
function mdTableHtml(table) {
  const t = document.createElement('table');
  const tbody = document.createElement('tbody');
  const mkRow = (cells, header) => {
    const tr = document.createElement('tr');
    for (const c of cells) {
      const cell = document.createElement('td');
      if (header) cell.innerHTML = `<strong>${escapeHtml(c)}</strong>`;
      else cell.textContent = c;
      tr.appendChild(cell);
    }
    return tr;
  };
  tbody.appendChild(mkRow(table.header, true));
  for (const r of table.rows) tbody.appendChild(mkRow(r, false));
  t.appendChild(tbody);
  return t;
}

// 纯文本 → HTML：非表格段每行一个 <p>，表格段换成 <table>。
function textWithMdTables(text) {
  const lines = text.split(/\r?\n/);
  const tables = scanMdTables(lines);
  if (!tables.length) return null;
  let out = '';
  let pos = 0;
  for (const t of tables) {
    for (const line of lines.slice(pos, t.start)) out += `<p>${escapeHtml(line)}</p>`;
    out += mdTableHtml(t).outerHTML;
    pos = t.end;
  }
  for (const line of lines.slice(pos)) out += `<p>${escapeHtml(line)}</p>`;
  return out;
}

// HTML → HTML：把「整块文本就是一张 markdown 表格」的块元素换成真 <table>。
// 只处理叶子块（含嵌套块的交给内层，避免父子重复替换）。
// 整块必须**就是**一张表：散文里夹着表不接管 —— 要保住正文的富文本就不能拆块，
// 那样一个 <pre> 里既有散文又有表格就会整块保持原样。
function htmlWithMdTables(html) {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  const blocks = Array.from(doc.body.querySelectorAll('p, pre, div, li'));
  let changed = false;
  for (const b of blocks) {
    if (blocks.some((o) => o !== b && b.contains(o))) continue;
    const txt = (b.textContent || '').replace(/\s+$/, '');
    if (!txt.includes('|')) continue;
    const lines = txt.split(/\r?\n/);
    const table = parseMdTableAt(lines, 0);
    if (!table || table.end !== lines.length) continue;
    const t = mdTableHtml(table);
    b.parentNode.replaceChild(doc.importNode(t, true), b);
    changed = true;
  }
  if (!changed) return null;
  return doc.body.innerHTML;
}

// 粘贴 markdown 表格 → 编辑器原生表格。
//
// 接管条件（两个都要满足）：
//   · 剪贴板里没有图片文件（有则交给 attachImageUpload 的守卫，否则两个
//     capture 监听都会 preventDefault，图片就传不上去了）
//   · 光标不在表格里：better-table 对表格单元格有专门的粘贴处理（过滤 op、
//     重建 cell-line、列宽平衡），我们一旦 preventDefault 就把它整个绕开，
//     「往单元格里粘贴」会变成「粘什么都插一张新表」。
function attachMarkdownTablePaste(el, editor) {
  el.addEventListener('paste', (e) => {
    const dt = e.clipboardData;
    if (!dt) return;
    const hasImage = dt.items && Array.from(dt.items)
      .some((it) => it.kind === 'file' && (it.type || '').startsWith('image/'));
    if (hasImage) return;

    const sel = editor.getSelection();
    if (sel) {
      const fmt = editor.getFormat(sel.index) || {};
      if (fmt['table-cell-line'] || fmt['table-col']) return;
    }

    const text = dt.getData('text/plain') || '';
    const html = dt.getData('text/html') || '';
    let newHtml = null;
    if (html) {
      newHtml = htmlWithMdTables(html);
      if (!newHtml) return;          // 没有 markdown 表格 → 交给 Quill 正常处理
    } else {
      newHtml = textWithMdTables(text);
      if (!newHtml) return;
    }

    let delta;
    try {
      // editor.clipboard.convert 是 Quill 公开的同步方法：走一遍 clipboard
      // matcher，把 HTML 变成 Delta。better-table 自带的 td/tr matcher 在这条
      // 路径上，表格因此是「模块自己认的」原生表格，而不是我们手搓的 ops。
      delta = editor.clipboard.convert({ html: newHtml, text: '' });
    } catch (err) {
      return;
    }
    if (!delta || !delta.ops || !delta.ops.length) return;

    e.preventDefault();
    e.stopPropagation();
    const index = sel ? sel.index : editor.getLength();
    const Delta = FluentEditor.imports && FluentEditor.imports.delta;
    const full = Delta ? new Delta().retain(index).concat(delta) : delta;
    editor.updateContents(full, 'user');
    try { editor.setSelection(index + delta.length(), 0); } catch (e2) { /* 位置算不准也不该挡住粘贴 */ }
  }, true);
}

// 只读渲染路径：把 Delta op 里的 markdown 表格拆成表格 ops。
// 表格 ops 同样走 clipboard matcher 拿，保证与编辑器产出的形状一致。
function splitOpsByMdTables(ops, editor) {
  const out = [];
  for (const op of ops) {
    const ins = op && op.insert;
    if (typeof ins !== 'string' || !ins.includes('|')) { out.push(op); continue; }
    const lines = ins.split('\n');
    const tables = scanMdTables(lines);
    if (!tables.length) { out.push(op); continue; }
    let pos = 0;
    for (const t of tables) {
      if (t.start > pos) {
        const text = lines.slice(pos, t.start).join('\n');
        if (text) out.push({ insert: text, attributes: op.attributes });
      }
      try {
        const tdelta = editor.clipboard.convert({ html: mdTableHtml(t).outerHTML, text: '' });
        for (const o of tdelta.ops) out.push(o);
      } catch (err) {
        // 转换不了就退回纯文本，不丢内容
        const text = lines.slice(t.start, t.end).join('\n');
        if (text) out.push({ insert: text, attributes: op.attributes });
      }
      pos = t.end;
    }
    if (pos < lines.length) {
      const text = lines.slice(pos).join('\n');
      if (text) out.push({ insert: text, attributes: op.attributes });
    }
  }
  return out;
}

// Safari 提交输入法时会把列表项拆掉（2026-10-22 Safari 26.2 + 系统拼音 trace）：
//   beforeinput deleteCompositionText  拼音 zhong 被删除
//   → 整个 LI（连同里面的拼音）被 WebKit 从 DOM 摘掉，列表容器里换成占位 <br>
//   → beforeinput insertFromComposition 汉字「中」被直接插到 <ol> 下
//   → Quill batchEnd 看到列表容器下的裸文本，收不进模型，最后把 <ol> 换成 <p>，
//     列表和文字一起消失。
//
// 主修复在 input deleteCompositionText 时（earlyRestore）：清掉游离 LI 里
// 残留的拼音、把它接回列表、把原生光标放进去，让随后的汉字落回 LI。
// 这里的函数是补漏：若汉字已经掉到 <ol>/<ul> 下（例如事件时序不如预期），
// 在 compositionend（Quill 的 batchEnd microtask 之前）把裸文本收回 LI。
function restoreEmptiedListItems(root, lineNode, listNode) {
  // 1) 优先复用组合开始时那一行的 LI 节点：Quill 处理「LI 被移除」的
  //    mutation 时会检查 removed node 是否仍连着文档，复用同一节点即可
  //    跳过 detach，原来的 LI blot 得以保留，后面的文本对账才能挂回去。
  if (
    lineNode && lineNode.nodeType === 1 && lineNode.tagName === 'LI' &&
    !root.contains(lineNode) && listNode && root.contains(listNode) &&
    (listNode.tagName === 'OL' || listNode.tagName === 'UL')
  ) {
    const strayText = [];
    const strayBr = [];
    listNode.childNodes.forEach((n) => {
      if (n.nodeType === 3 && n.data.length > 0) strayText.push(n);
      else if (n.nodeType === 1 && n.tagName === 'BR') strayBr.push(n);
    });
    if (strayText.length > 0) {
      listNode.insertBefore(lineNode, strayText[0]);
      strayText.forEach((n) => lineNode.appendChild(n));
      strayBr.forEach((n) => n.remove());
      return true;
    }
  }
  // 2) 兜底：没有记录到原 LI 时，把列表容器下的裸文本包进一个新 LI。
  let restored = false;
  root.querySelectorAll('ol, ul').forEach((list) => {
    const stray = [];
    list.childNodes.forEach((n) => {
      if (n.nodeType === 3 && n.data.length > 0) stray.push(n);
      else if (n.nodeType === 1 && n.tagName === 'BR') stray.push(n);
    });
    const strayText = stray.filter((n) => n.nodeType === 3);
    if (strayText.length === 0) return;
    const type = (list.className || '').trim().split(/\s+/)[0] || 'ordered';
    const li = document.createElement('li');
    li.className = type;
    list.insertBefore(li, strayText[0]);
    strayText.forEach((n) => li.appendChild(n));
    stray.filter((n) => n.nodeType === 1).forEach((n) => n.remove());
    restored = true;
  });
  return restored;
}

// 输入法组合文本兜底。
//
// 两类已知丢字路径都会让「DOM 里有中文、Quill 的模型里没有」：
//   1) Safari 提交时拆掉空 LI（见上），汉字落到列表容器上；
//   2) 浏览器在组合输入期间丢 MutationRecord（slab/quill#3837、#3969），
//      或者组合文本被塞进 `.ql-ui` 标记节点（勾选列表仍保留该节点）——
//      blot 树/Delta 里没有这段文字，下一次 update 就会被删掉。
//
// 三层处理：
//   - input deleteCompositionText / insertFromComposition：提前把 LI 接回去、
//     放好光标（主修复，见 earlyRestore）；
//   - compositionend 同步阶段：补漏恢复被拆掉的列表结构；
//   - compositionend 的 setTimeout：把 ql-ui 里的孤儿文本挪回 li，并扫一遍
//     文本节点：未注册成 blot 的、以及 text blot 的 text 与 DOM 不一致的，
//     合成 MutationRecord 交给 scroll.update()，让 Quill 把文字收进模型。
// 正常输入时这些都无事可做（DOM 与模型一致），不会产生额外改动。
function repairCompositionText(el, editor) {
  // 每个容器只绑一次；节点被复用、create 再来一遍时不叠加监听。
  if (el.__rodeo_composition_repair) return;
  el.__rodeo_composition_repair = true;
  // 组合开始时记下当前行的 DOM 节点与所属列表容器：Safari 会在提交时
  // 把这个 LI 从 DOM 里摘掉，之后要靠同一节点把它接回去。
  let composingLine = null;
  let composingList = null;
  // Safari 的提交是 deleteCompositionText → insertFromComposition 两步。
  // 在 delete 的 input 事件时把被摘掉的 LI 提前接回去（并放好原生光标），
  // 让随后那次插入落回列表项，而不是落到 <ol> 上。
  const earlyRestore = () => {
    try {
      const root = el.querySelector('.ql-editor');
      if (!root) return false;
      const li = composingLine;
      const list = composingList;
      if (!li || li.nodeType !== 1 || li.tagName !== 'LI') return false;
      if (!list || !root.contains(list)) return false;
      if (list.tagName !== 'OL' && list.tagName !== 'UL') return false;
      if (list.contains(li)) return true;
      // Safari 删除拼音的做法是把整个（变空的）LI 从 DOM 摘掉——拼音文本
      // 还留在这个游离节点里。直接原样接回去会把它复活成「中zhong」；
      // 清掉残留（保留勾选列表的 ql-ui 标记节点），换成可编辑占位。
      Array.from(li.childNodes).forEach((n) => {
        if (!(n.nodeType === 1 && n.classList && n.classList.contains('ql-ui'))) {
          n.remove();
        }
      });
      if (!li.firstChild) li.appendChild(document.createElement('br'));
      list.appendChild(li);
      try {
        const r = document.createRange();
        r.setStart(li, 0);
        r.collapse(true);
        const s = window.getSelection();
        s.removeAllRanges();
        s.addRange(r);
      } catch (e) {
        // 光标放不进去也至少把结构恢复了。
      }
      return true;
    } catch (e) {
      return false;
    }
  };
  el.addEventListener('input', (ev) => {
    if (ev.inputType === 'deleteCompositionText') {
      earlyRestore();
    } else if (ev.inputType === 'insertFromComposition') {
      // 如果 delete 时没能把 LI 接回去，Safari 会把汉字插到 <ol> 下；
      // 在输入事件里立刻把它收回 LI，赶在 compositionend 之前。
      try {
        const root = el.querySelector('.ql-editor');
        if (root) restoreEmptiedListItems(root, composingLine, composingList);
      } catch (e) {
        // 忽略：compositionend 还有一次兜底。
      }
    }
  });
  el.addEventListener('beforeinput', (ev) => {
    if (ev.inputType === 'insertFromComposition') earlyRestore();
  });
  el.addEventListener('compositionstart', () => {
    composingLine = null;
    composingList = null;
    try {
      const sel = editor.getSelection();
      if (!sel) return;
      const [line] = editor.getLine(sel.index);
      if (line && line.domNode) {
        composingLine = line.domNode;
        composingList = line.domNode.parentNode;
      }
    } catch (e) {
      composingLine = null;
      composingList = null;
    }
  });
  el.addEventListener('compositionend', () => {
    // 同步阶段：必须在 Quill 的 batchEnd（queueMicrotask）之前完成。
    try {
      const root = el.querySelector('.ql-editor');
      if (root) restoreEmptiedListItems(root, composingLine, composingList);
    } catch (e) {
      // 结构恢复失败也不能挡住输入。
    }
    composingLine = null;
    composingList = null;
    // fluent-editor 自己的 compositionend 处理走 queueMicrotask，
    // setTimeout 排在其后，避免和它的 batchEnd 抢时序。
    setTimeout(() => {
      const root = el.querySelector('.ql-editor');
      if (!root) return;
      let moved = false;
      root.querySelectorAll('li > .ql-ui').forEach((ui) => {
        const text = ui.textContent;
        if (!text) return;
        ui.textContent = '';
        ui.parentNode.appendChild(document.createTextNode(text));
        moved = true;
      });
      try {
        const scroll = editor.scroll;
        const mutations = [];
        const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, null);
        let node;
        while ((node = walker.nextNode())) {
          const parent = node.parentNode;
          if (!parent || !root.contains(parent)) continue;
          // ql-ui 里的内容不是正文；上面已经挪走，这里跳过残留。
          if (parent.nodeType === 1 && parent.closest('.ql-ui')) continue;
          const blot = scroll.find(node, false);
          if (blot) {
            // 已注册的 text blot：浏览器改了 DOM 但记录丢了 → 同步 text。
            if (typeof blot.text === 'string' && blot.text !== node.data) {
              mutations.push({ type: 'characterData', target: node });
            }
          } else if (scroll.find(parent, true)) {
            // 未注册的文本节点：当成一次 childList 插入补交给 Quill。
            mutations.push({
              type: 'childList',
              target: parent,
              addedNodes: [node],
              removedNodes: [],
            });
          }
        }
        if (mutations.length) {
          scroll.update(mutations);
        } else if (moved) {
          editor.update();
        }
      } catch (e) {
        // 修复失败不阻塞后续输入；文字仍留在 DOM 里。
      }
    }, 0);
  });
}

// 只读渲染复用同一个离屏实例；整页评论共用一个，与评论条数无关。
let renderHost = null;
let renderEditor = null;

window.__rodeo_tiny_editor__ = {
  // 创建编辑器实例。deltaJson 为已归一化的 Quill Delta JSON 字符串（可为空串）。
  // opts.upload(file) -> Promise<string>：粘贴/拖入图片时的上传函数，返回可插入的 URL。
  // opts.readOnly=true：仅展示，不要工具栏、不要粘贴上传、不要 on_change（匿名分享页用）。
  create(el, deltaJson, opts) {
    cleanupAux(el);
    sweepOrphans();
    // `.tiny-editor` 由这里加，而不是写在 Rust 的 view! 里：Leptos 重渲染
    // 时会把节点上它管理的 class 属性整体重写为模板里的值，Quill 挂上的
    // `ql-container ql-snow` 会被一并抹掉——表现为评论一次之后正文编辑器
    // 边框消失、暗色下背景变黑。class 交给 JS 管，Quill 的类就没人动了。
    el.classList.add('tiny-editor');
    // 再清空容器，避免复用节点时叠加旧内容。
    el.innerHTML = '';
    // 只读模式：跳过会产生工具栏/上传监听/绑定 on_change 的全部支路。
    // 必须放在任何 use-before-init 之前；read_only 路径不向外界暴露任何副作用。
    const readOnly = !!(opts && opts.readOnly);
    if (readOnly) {
      const readOnlyEditor = new FluentEditor(el, {
        theme: 'snow',
        placeholder: '',
        readOnly: true,
        modules: {
          toolbar: false,
          // 只读视图也要高亮代码块（分享页的条目正文走这条路）。
          // window.hljs 判空同可编辑分支：syntax 模块缺 hljs 会抛异常，
          // 抛在 new FluentEditor 里会让整个编辑器挂不上。
          ...syntaxModule(),
        },
      });
      // 标记只读：syntax 模块会把语言下拉（<select>）作为 code-block 的 uiNode
      // 挂进内容里，只读视图不该有可交互控件。用 CSS 藏掉它
      // （见 style/main.css 的 .ql-readonly select）。
      el.classList.add('ql-readonly');
      if (deltaJson) {
        try {
          readOnlyEditor.setContents(normalizeListAttributes(JSON.parse(deltaJson)), 'silent');
        } catch (e) {
          // 非法 Delta 走纯文本回退——只读场景里允许「展示成段落文本」。
          try {
            readOnlyEditor.setText(deltaJson, 'silent');
          } catch (e2) {
            // 最坏情况忽略，编辑器保持空内容。
          }
        }
      }
      el.__rodeo_editor = readOnlyEditor;
      return readOnlyEditor;
    }
    // 记下这次新建过程中新增的兄弟节点（工具栏/浮层），供下次创建时精确清理。
    const before = new Set(el.parentNode.children);
    // @ 提及：fluent-editor 3.x 的 mention 模块只接受 `search(term) -> item[]`，
    // 不认 `mentionList` / `enabled` / `spaceAfterInsert`——这些字段在源码里
    // 完全没被读到，传过去也是 silently merged 进去、被默认项覆盖。**没有
    // search 时模块构造函数直接 return，键盘绑定和下拉 DOM 都不创建**，
    // 所以控制台才会出现 "please provide a search function!" 的警告。
    //
    // 这里把成员名单快照成闭包变量，再写一个简单的「按 name 前缀过滤、
    // 大小写无关」的 search，候选项沿用 {id, name, ...} 的形态，让默认的
    // `renderMentionItem` / `renderMentionText` 都能直接用。
    //
    // 名单为空时不传 `mention` 模块，让编辑器走纯文本路径——后端
    // `service/mention.rs::extract_mention_names` 仍能从纯文本里抓
    // `@姓名` 触发通知，UI 侧只是没了下拉提示。
    const rawMembers = (opts && opts.members) || [];
    let memberItems = [];
    try {
      const parsed = typeof rawMembers === 'string' ? JSON.parse(rawMembers) : rawMembers;
      if (Array.isArray(parsed)) {
        memberItems = parsed
          .map((m) => ({
            id: m.account_id || m.accountId || m.id,
            name: m.name || '',
            avatar: m.email || '',
          }))
          .filter((m) => m.id && m.name);
      }
    } catch (e) {
      memberItems = [];
    }
    const mentionModule = memberItems.length > 0
      ? {
          // 空查询返回前 8 个；非空按 name 前缀过滤（lower-case 比较）。
          // 列表已在此闭包里，不需要再走 JSON.parse，每按一次 @ 都跑一遍。
          search(term) {
            const t = (term || '').toLowerCase();
            if (!t) return memberItems.slice(0, 8);
            return memberItems.filter((m) => m.name.toLowerCase().includes(t)).slice(0, 8);
          },
        }
      : false;
    // syntax 模块（Quill 自带的）：给 code-block 挂语言下拉 + highlight.js 高亮。
    //
    // 两个坑：
    // 1. 它在构造函数里检查 `options.hljs == null` 就抛异常
    //    （"Syntax module requires highlight.js"）。抛在 new FluentEditor 里
    //    会让整个编辑器不出现（正文区空白），所以必须先判 window.hljs。
    // 2. fluent-editor.mjs 在**模块求值时**就执行 `Ji.DEFAULTS = { hljs: window.hljs }`，
    //    所以 hljs 必须在那个 bundle 求值之前就在页面上——app.rs 里把
    //    /tiny-editor/hljs.min.js 排在 glue.js 之前就是为此。
    //
    // 显式传 hljs 而不是依赖 DEFAULTS 的快照：将来万一调整了 script 顺序，
    // DEFAULTS.hljs 变 undefined，这里传的值仍然兜得住。
    const modules = {
      ...syntaxModule(),
      toolbar: [
        ['bold', 'italic', 'underline', 'strike'],
        [{ header: 1 }, { header: 2 }, { header: 3 }],
        [{ list: 'ordered' }, { list: 'bullet' }],
        ['blockquote', 'code-block'],
        ['link'],
        // better-table 模块本身已经在跑（themes/snow 的 DEFAULTS.modules 里
        // 带了 "better-table": {operationMenu:{color:true}}），只差工具栏这一行。
        //
        // 这里不用再写 'better-table': true —— 主题默认已经带了，写了也没坏处
        //（Quill 把 true 映射成 {}，lodash merge 用空对象做源不会覆盖目标，
        // 主题的 operationMenu 配置原样保留），但主题默认那份更完整，不必重复。
        ['better-table'],
        ['clean'],
      ],
    };
    // 成员名单空就不挂 mention 模块——fluent-editor 收到一个没有 search
    // 字段的 options 会在控制台 warn "please provide a search function!"，
    // 而且构造函数会直接 return，键盘绑定和下拉 DOM 都不会创建。
    if (mentionModule) modules.mention = mentionModule;
    const editor = new FluentEditor(el, {
      theme: 'snow',
      placeholder: '输入详情…',
      modules,
    });
    el.__rodeo_aux = Array.from(el.parentNode.children).filter((n) => !before.has(n));
    el.__rodeo_aux.forEach((n) => auxOwners.set(n, el));
    attachImageUpload(el, editor, opts);
    attachMarkdownTablePaste(el, editor);
    repairCompositionText(el, editor);

    if (deltaJson) {
      try {
        editor.setContents(normalizeListAttributes(JSON.parse(deltaJson)), 'silent');
      } catch (e) {
        // 非法 Delta 时忽略，编辑器保持空内容。
      }
    }

    // 同一次响应式更新里可能还有别处的编辑器被卸载，卸载与新建的先后顺序
    // 由 Leptos 决定。等这次更新彻底落定后再扫一遍，漏网的孤儿也一并清掉。
    setTimeout(sweepOrphans, 0);

    return editor;
  },

  // 落库前把列表属性统一成 Quill 标准字符串形状。解析失败就原样返回，
  // 不因为一次归一化失败把用户刚敲的内容吞掉。
  normalizeDeltaJson(deltaJson) {
    try {
      return JSON.stringify(normalizeListAttributes(JSON.parse(deltaJson)));
    } catch (e) {
      return deltaJson;
    }
  },

  // 把 Delta JSON 渲染成 HTML，供评论列表这类只读场景使用。
  // 复用同一个离屏实例：整页评论只占一个编辑器对象，按条新建既慢，
  // 又会和工具栏清理互相干扰。
  toHtml(deltaJson) {
    if (!deltaJson) return '';
    if (!renderEditor) {
      renderHost = document.createElement('div');
      renderHost.style.display = 'none';
      document.body.appendChild(renderHost);
      renderEditor = new FluentEditor(renderHost, {
        readOnly: true,
        modules: {
          toolbar: false,
          // 渲染实例用 default theme，没有 better-table；table-view blot 的
          // scroll 处理器 getModule('better-table') 缺模块会 TypeError。
          'better-table': true,
          // 只读实例也要挂 syntax，否则评论里的代码块导出出来是纯文本。
          // window.hljs 判空同可编辑分支：syntax 模块缺 hljs 会抛异常。
          ...syntaxModule(),
        },
      });
    }
    try {
      // 兜底条件是「解析不出 Delta」而不只是「JSON.parse 失败」：老正文可能
      // 恰好是 123、"foo"、{"a":1} —— JSON.parse 成功但不是 Delta 形状，
      // setContents 会抛错，还是落进 catch → 空白。所以判定写成：
      // parse 失败 **或** 结果不是 {ops:[…]} / [...] 形状 → 一律按纯文本兜底。
      // 这一步顺带修掉一个现存缺陷：老评论存的是纯文本，现在 toHtml 直接
      // catch 返回空串，评论正文在只读视图里是空白。
      let ops;
      try {
        const parsed = JSON.parse(deltaJson);
        const arr = parsed && Array.isArray(parsed.ops) ? parsed.ops
          : (Array.isArray(parsed) ? parsed : null);
        if (!arr) throw new Error('不是 Delta 形状');
        ops = normalizeListAttributes({ ops: arr }).ops;
      } catch (e) {
        ops = [{ insert: String(deltaJson) }];
      }
      // 老正文里的 markdown 表格（纯文本形态）拆成表格 ops。
      ops = splitOpsByMdTables(ops, renderEditor);
      renderEditor.setContents({ ops }, 'silent');
      // syntax 模块的高亮是 debounce 1s 的（SCROLL_OPTIMIZE 触发），
      // 刚 setContents 就导出会拿到没高亮的纯文本。这里手动同步跑一次。
      const syntax = renderEditor.getModule('syntax');
      if (syntax && typeof syntax.highlight === 'function') {
        try { syntax.highlight(); } catch (e) { /* 高亮失败不影响导出 */ }
      }
      // 导出窗口：只在这段同步调用里让 ListBlot.formats() 返回字符串，
      // 导出器才能把相邻列表项归进同一个 <ol>/<ul>。
      exportingSemantic = true;
      try {
        let html = renderEditor.getSemanticHTML();
        // 语言下拉是 code-block 的 uiNode，正常不进 getSemanticHTML 的输出；
        // 万一进去了也不能漏到只读视图里。
        html = html.replace(/<select\b[\s\S]*?<\/select>/gi, '');
        return html;
      } finally {
        exportingSemantic = false;
      }
    } catch (e) {
      return '';
    }
  },
};
