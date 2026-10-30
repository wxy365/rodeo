// rodeo 的 Tiptap 编辑器桥接（ES module）。
//
// 由 app shell 以 <script type="module" src="/tiptap/glue.js"> 加载。
// 暴露 window.__rodeo_editor__.create(el, html, opts) 给 Rust/WASM 侧调用。
//
// 边界协议是 **HTML**：Tiptap 原生吃 HTML，存库也是 HTML，两边不用再翻译。
// （旧编辑器 Quill 说的是 Delta，转换在服务端的 richtext::delta_to_html。）

import {
  Editor,
  StarterKit,
  TableKit,
  Image,
  Link,
  Mention,
  Placeholder,
  TaskList,
  TaskItem,
  CodeBlockLowlight,
  Highlight,
  createLowlight,
  common,
} from './editor.mjs';
import { createToolbar } from './toolbar.js';

const lowlight = createLowlight(common);

// ── markdown 表格 ────────────────────────────────────────────────────
// 识别规则（Rust 侧 src/richtext.rs 有一份等价实现，两边必须逐条一致，
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

// 取一个块元素的纯文本，**`<br>` 算换行**。
// 不能用 textContent：它只拼文本节点，`<br>` 一个字符都不贡献 ——
// Word / Google Docs 复制出来的表格是 `<p>| a | b |<br>| --- | --- |…</p>`，
// 用 textContent 会拼成一行，表格就认不出来。
function blockText(el) {
  let out = '';
  for (const node of el.childNodes) {
    if (node.nodeType === 3) { out += node.data; continue; }
    if (node.nodeType !== 1) continue;
    const tag = node.tagName.toLowerCase();
    if (tag === 'br') { out += '\n'; continue; }
    out += blockText(node);
    if (['p', 'div', 'li', 'pre', 'tr', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6',
      'blockquote', 'ul', 'ol', 'table', 'section', 'article'].includes(tag)) {
      out += '\n';
    }
  }
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
    const txt = blockText(b).replace(/\s+$/, '');
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

/**
 * @提及的下拉。
 *
 * `opts.members` 是 Rust 侧传的成员名单（[{ id, name }]）。Tiptap 的 mention
 * 扩展基于 @tiptap/suggestion：items 出候选，render 出浮层。浮层自己写
 * （不引 tippy.js），定位用 suggestion 给的 clientRect。
 */
function mentionSuggestion(members) {
  return {
    char: '@',
    items: ({ query }) => {
      const q = (query || '').toLowerCase();
      return members
        .filter((m) => (m.name || '').toLowerCase().includes(q))
        .slice(0, 8);
    },
    render: () => {
      let el = null;
      let items = { items: [], command: () => {} };
      let selected = 0;

      const renderList = () => {
        if (!el) return;
        el.textContent = '';
        if (!items.items.length) {
          el.hidden = true;
          return;
        }
        el.hidden = false;
        items.items.forEach((item, i) => {
          const row = document.createElement('button');
          row.type = 'button';
          row.className = 'tiptap-mention-item' + (i === selected ? ' is-active' : '');
          row.textContent = item.name;
          row.addEventListener('mousedown', (e) => {
            e.preventDefault();
            items.command({ id: item.id, label: item.name });
          });
          el.appendChild(row);
        });
      };

      const position = (clientRect) => {
        if (!el) return;
        const rect = typeof clientRect === 'function' ? clientRect() : clientRect;
        if (!rect) return;
        el.style.left = `${rect.left}px`;
        el.style.top = `${rect.bottom + 4}px`;
      };

      return {
        onStart(props) {
          el = document.createElement('div');
          el.className = 'tiptap-mention';
          document.body.appendChild(el);
          items = props;
          selected = 0;
          renderList();
          position(props.clientRect);
        },
        onUpdate(props) {
          items = props;
          selected = Math.max(0, Math.min(selected, items.items.length - 1));
          renderList();
          position(props.clientRect);
        },
        onKeyDown({ event }) {
          if (event.key === 'ArrowUp' && items.items.length) {
            selected = (selected + items.items.length - 1) % items.items.length;
            renderList();
            return true;
          }
          if (event.key === 'ArrowDown' && items.items.length) {
            selected = (selected + 1) % items.items.length;
            renderList();
            return true;
          }
          if (event.key === 'Enter' && items.items.length) {
            const item = items.items[selected];
            if (item) items.command({ id: item.id, label: item.name });
            return true;
          }
          if (event.key === 'Escape') {
            if (el) { el.remove(); el = null; }
            return true;
          }
          return false;
        },
        onExit() {
          if (el) { el.remove(); el = null; }
        },
      };
    },
  };
}

/**
 * Tab 键：在正文里插入制表符。
 *
 * Tiptap 的默认键位只给列表项（缩进）和表格单元（跳下一格）绑了 Tab，
 * 段落 / 标题 / 引用 / 代码块里没人接 —— 浏览器默认行为把焦点移到下一个
 * 可聚焦元素（工具栏的粗体按钮），正文里一个字符都插不进去。
 *
 * 列表和表格之外按 Tab 插入 '\t'（Quill 时代就是这个行为）。Shift-Tab 不
 * 接管，留给浏览器往回移焦点（列表里的 Shift-Tab 是 Tiptap 自己的反缩进）。
 */
function handleTabKey(editor, event) {
  if (!editor || event.key !== 'Tab' || event.shiftKey) return false;
  // 列表项缩进、表格跳下一格：交回 Tiptap 自己的键位
  if (editor.isActive('listItem') || editor.isActive('taskItem')) return false;
  if (editor.isActive('tableCell') || editor.isActive('tableHeader')) return false;
  event.preventDefault();
  const { state, dispatch } = editor.view;
  dispatch(state.tr.insertText('\t'));
  return true;
}

/**
 * 粘贴的处理：图片走上传，markdown 表格转成原生表格。
 *
 * 图片优先：剪贴板里同时有图片和文字时，图片赢（Quill 时代就是这个优先级）。
 */
function handlePaste(view, event, editor) {
  const dt = event.clipboardData;
  if (!dt || !editor) return false;

  // 图片：走上传，插进正文（图片是正文的一部分，不列进附件列表）。
  const files = Array.from(dt.files || []).filter((f) => (f.type || '').startsWith('image/'));
  if (files.length) {
    event.preventDefault();
    uploadIntoEditor(editor, editor.__rodeo_upload, files);
    return true;
  }

  return handleMarkdownTablePaste(view, event, editor);
}

/**
 * 串行上传并插入：每个文件上传完成后再动下一个，位置由前一个的结果决定，
 * 避免并发 await 让插入点互相错位。占位文本在上传完成后被替换掉。
 */
async function uploadIntoEditor(editor, upload, files) {
  if (typeof upload !== 'function') return;
  let index = editor.state.selection.from;
  for (const file of files) {
    const at = index;
    const placeholder = '上传中…';
    editor.chain().focus().insertContentAt(at, placeholder).run();
    index = at + placeholder.length;
    try {
      const url = await upload(file);
      editor
        .chain()
        .focus()
        .deleteRange({ from: at, to: index })
        .insertContentAt(at, { type: 'image', attrs: { src: url } })
        .run();
      index = at + 1;
    } catch (err) {
      editor.chain().focus().deleteRange({ from: at, to: index }).run();
      const fail = '图片上传失败';
      editor.chain().focus().insertContentAt(at, fail).run();
      index = at + fail.length;
      console.warn('图片上传失败', err);
    }
  }
}

/**
 * 粘贴 markdown 表格 → 编辑器原生表格。
 *
 * 接管条件：光标不在表格里 —— 表格内粘贴有专门的列宽平衡等处理，抢过来会坏掉。
 *
 * 转换方式：把 markdown 表格换成真 `<table>` 的 HTML，交给 Tiptap 自己解析
 * （table 扩展认识它，出来的是原生表格节点）。
 */
function handleMarkdownTablePaste(view, event, editor) {
  const dt = event.clipboardData;
  if (!dt || !editor) return false;

  // 光标在表格里就放行 —— 表格内粘贴有专门的列宽平衡等处理。
  if (editor.isActive('table')) return false;

  const text = dt.getData('text/plain') || '';
  const html = dt.getData('text/html') || '';
  let newHtml = null;
  if (html) {
    newHtml = htmlWithMdTables(html);
    if (!newHtml) return false;
  } else {
    newHtml = textWithMdTables(text);
    if (!newHtml) return false;
  }

  event.preventDefault();
  editor.chain().focus().insertContentAt(editor.state.selection.from, newHtml).run();
  return true;
}

window.__rodeo_editor__ = {
  /**
   * 在 el 上创建编辑器。
   *
   * @param {HTMLElement} el 宿主节点
   * @param {string} html 初始内容（HTML）
   * @param {object} opts { readOnly, upload(file)->Promise<url>, members }
   */
  create(el, html, opts = {}) {
    const readOnly = !!opts.readOnly;
    const members = Array.isArray(opts.members) ? opts.members : [];

    // 编辑器实例在构造之后才有；粘贴处理要用它（insertContentAt 吃 HTML 字符串），
    // 所以用可变引用在调用时取。
    let editorRef = null;

    const editor = new Editor({
      element: el,
      extensions: [
        StarterKit.configure({
          // 代码块用 lowlight 版（带语法高亮与语言）
          codeBlock: false,
          // 链接单独配（openOnClick 等）
          link: false,
          heading: { levels: [1, 2, 3] },
        }),
        Link.configure({
          openOnClick: false,
          autolink: true,
          protocols: ['http', 'https', 'mailto'],
        }),
        Highlight.configure({ multicolor: false }),
        TableKit.configure({ resizable: true }),
        Image.configure({ allowBase64: false, inline: false }),
        TaskList,
        TaskItem.configure({ nested: true }),
        CodeBlockLowlight.configure({ lowlight, defaultLanguage: null }),
        Placeholder.configure({ placeholder: '输入详情…' }),
        Mention.configure({
          HTMLAttributes: { class: 'tiptap-mention-chip' },
          suggestion: mentionSuggestion(members),
        }),
      ],
      content: html || '',
      editable: !readOnly,
      // 空白全保留：默认策略会把段落里的制表符压成空格 —— 正文里按 Tab
      // 插入的 '\t' 存进库再打开就没了。块之间的空白两种策略结果一致
      // （实测存量正文解析结果逐字节相同），所以开这个开关没有副作用。
      parseOptions: { preserveWhitespace: 'full' },
      editorProps: {
        attributes: { class: 'tiptap-content' },
        // 粘贴：图片走上传，markdown 表格转成原生表格。
        // 表格规则与服务端 src/richtext.rs 一致。
        handlePaste: (view, event) => handlePaste(view, event, editorRef),
        // Tab：正文里插入制表符（列表缩进 / 表格跳格除外）。
        handleKeyDown: (view, event) => handleTabKey(editorRef, event),
      },
    });

    editorRef = editor;
    // 上传回调挂在 editor 上：粘贴处理要拿到它。
    editor.__rodeo_upload = opts.upload;
    el.__rodeo_editor = editor;

    if (!readOnly) {
      const { bar, tableBar, langSel } = createToolbar(editor, { upload: opts.upload });
      const host = el.parentNode;
      if (host) {
        // 工具栏是编辑器容器的兄弟节点（跟 Quill 时代一样）：宿主重渲染时
        // 容器可能被换掉，工具栏就成了孤儿 —— 挂新之前先把旧的清掉。
        //
        // **不要在 host 上加类**：host 是 Leptos 渲染的，重渲染时它的 class
        // 属性会被整体重写成模板里的值，加的类就没了。
        const oldBar = host.querySelector(':scope > .tiptap-toolbar');
        if (oldBar) oldBar.remove();
        const oldTable = host.querySelector(':scope > .tiptap-tablebar');
        if (oldTable) oldTable.remove();
        const oldLang = host.querySelector(':scope > .tiptap-cb-lang');
        if (oldLang) oldLang.remove();
        host.insertBefore(bar, el);
        host.appendChild(tableBar);
        // 代码块语言下拉：浮在代码块右上角，默认藏起来。
        host.appendChild(langSel);
      }
    }

    return editor;
  },
};
