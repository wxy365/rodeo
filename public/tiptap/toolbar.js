// Tiptap 工具栏（vanilla，无框架）。
//
// Tiptap 是 headless 的 —— 它不给界面，工具栏整条都要自己写。这是「从 Quill
// 换到 Tiptap」里工作量最大的一块，也是「编辑器像不像样」的直接决定因素。
//
// 两条原则：
//   · 激活态用 `editor.isActive(...)` 驱动，不另存状态；
//   · 表格的 7 个操作不进主工具栏（会把工具栏撑爆），做成光标在表格内时
//     浮在表格上方的浮动条。

/**
 * 代码块语言清单。key 必须与 lowlight 注册的语言名一致（Tiptap 的
 * code-block-lowlight 用 lowlight.highlight({language})，名字不对会抛）。
 * 这份清单与编辑器侧低亮度的 23 项保持一致。
 */
const CODE_LANGUAGES = [
  ['', '纯文本'],
  ['bash', 'Bash'],
  ['c', 'C'],
  ['cpp', 'C++'],
  ['csharp', 'C#'],
  ['css', 'CSS'],
  ['diff', '差异'],
  ['go', 'Go'],
  ['java', 'Java'],
  ['javascript', 'JavaScript'],
  ['json', 'JSON'],
  ['markdown', 'Markdown'],
  ['php', 'PHP'],
  ['python', 'Python'],
  ['ruby', 'Ruby'],
  ['rust', 'Rust'],
  ['sql', 'SQL'],
  ['typescript', 'TypeScript'],
  ['xml', 'HTML/XML'],
  ['yaml', 'YAML'],
];

/** 一组按钮的定义。[cmd, 标题, 图标字符, 是否是开关型] */
const GROUPS = [
  [
    ['bold', '粗体', 'B', true],
    ['italic', '斜体', 'I', true],
    ['underline', '下划线', 'U', true],
    ['strike', '删除线', 'S', true],
    ['code', '行内代码', '<>', true],
  ],
  [
    ['h1', '标题 1', 'H1', true],
    ['h2', '标题 2', 'H2', true],
    ['h3', '标题 3', 'H3', true],
  ],
  [
    ['bulletList', '无序列表', '•', true],
    ['orderedList', '有序列表', '1.', true],
    ['taskList', '任务列表', '☑', true],
  ],
  [
    ['blockquote', '引用', '❝', true],
    ['codeBlock', '代码块', '</>', true],
    ['horizontalRule', '分割线', '—', false],
  ],
  [
    ['link', '链接', '🔗', false],
    ['image', '图片', '🖼', false],
    ['table', '插入表格', '⊞', false],
  ],
  [
    ['undo', '撤销', '↶', false],
    ['redo', '重做', '↷', false],
  ],
];

/**
 * 表格浮动条的操作。[cmd, 标题, 图标字符或内联 SVG]
 *
 * 图标语义：
 *   · 「插入」用带方向的箭头（往下加行、往右加列），一眼能看出加在哪边；
 *   · 三个「删除」必须**一眼可分**，不能都画成 ✕（纯图标的一排里，
 *     同形图标只能靠悬停 tooltip 区分，等于没有区分）：
 *       - 删除行：表格外框 + 横向分格，中间那行整条高亮，行内一个小 ✕；
 *       - 删除列：同一外框 + 纵向分格，中间那列整条高亮，列内一个小 ✕；
 *       - 删除表格：整张表格被一个大 ✕ 划掉。
 *     行 / 列的差别落在「高亮带的方向」上（横带 = 行，竖带 = 列）。
 *   · 合并 / 拆分用 ⿴ / ⿲（纯文字符号，语义靠 tooltip 补足）。
 *
 * 图标是 15px 的内联 SVG，`stroke="currentColor"` 跟随按钮文字颜色，
 * 悬停时的 danger 色由 CSS 给（见 public/tiptap/style.css）。
 */
const ICON_DELETE_ROW = `<svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-linecap="round" aria-hidden="true">
<rect x="1.9" y="1.9" width="12.2" height="12.2" rx="2.2" stroke-width="1.2"/>
<path d="M1.9 5.4h12.2M1.9 10.6h12.2" stroke-width=".9" opacity=".45"/>
<rect x="2.9" y="6" width="10.2" height="4" rx=".8" fill="currentColor" stroke="none" opacity=".2"/>
<path d="M6.8 6.9l2.4 2.2M9.2 6.9l-2.4 2.2" stroke-width="1.4"/>
</svg>`;

const ICON_DELETE_COLUMN = `<svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-linecap="round" aria-hidden="true">
<rect x="1.9" y="1.9" width="12.2" height="12.2" rx="2.2" stroke-width="1.2"/>
<path d="M5.4 1.9v12.2M10.6 1.9v12.2" stroke-width=".9" opacity=".45"/>
<rect x="6" y="2.9" width="4" height="10.2" rx=".8" fill="currentColor" stroke="none" opacity=".2"/>
<path d="M6.9 6.8l2.2 2.4M9.1 6.8l-2.2 2.4" stroke-width="1.4"/>
</svg>`;

const ICON_DELETE_TABLE = `<svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-linecap="round" aria-hidden="true">
<rect x="1.9" y="1.9" width="12.2" height="12.2" rx="2.2" stroke-width="1.2"/>
<path d="M1.9 5.4h12.2M1.9 10.6h12.2M5.4 1.9v12.2M10.6 1.9v12.2" stroke-width=".9" opacity=".35"/>
<path d="M3.4 3.4l9.2 9.2M12.6 3.4l-9.2 9.2" stroke-width="1.6"/>
</svg>`;

const TABLE_ACTIONS = [
  ['addRowAfter', '下方插入行', '⤓'],
  ['deleteRow', '删除本行', ICON_DELETE_ROW],
  ['addColumnAfter', '右侧插入列', '⇥'],
  ['deleteColumn', '删除本列', ICON_DELETE_COLUMN],
  ['mergeCells', '合并单元格', '⿴'],
  ['splitCell', '拆分单元格', '⿲'],
  ['deleteTable', '删除表格', ICON_DELETE_TABLE],
];

/** 命令 → Tiptap chain 调用。返回 true 表示执行了。 */
function runCommand(editor, cmd) {
  const c = editor.chain().focus();
  switch (cmd) {
    case 'bold': c.toggleBold().run(); return true;
    case 'italic': c.toggleItalic().run(); return true;
    case 'underline': c.toggleUnderline().run(); return true;
    case 'strike': c.toggleStrike().run(); return true;
    case 'code': c.toggleCode().run(); return true;
    case 'h1': c.toggleHeading({ level: 1 }).run(); return true;
    case 'h2': c.toggleHeading({ level: 2 }).run(); return true;
    case 'h3': c.toggleHeading({ level: 3 }).run(); return true;
    case 'bulletList': c.toggleBulletList().run(); return true;
    case 'orderedList': c.toggleOrderedList().run(); return true;
    case 'taskList': c.toggleTaskList().run(); return true;
    case 'blockquote': c.toggleBlockquote().run(); return true;
    case 'codeBlock': c.toggleCodeBlock().run(); return true;
    case 'horizontalRule': c.setHorizontalRule().run(); return true;
    case 'undo': c.undo().run(); return true;
    case 'redo': c.redo().run(); return true;
    case 'addRowAfter': c.addRowAfter().run(); return true;
    case 'deleteRow': c.deleteRow().run(); return true;
    case 'addColumnAfter': c.addColumnAfter().run(); return true;
    case 'deleteColumn': c.deleteColumn().run(); return true;
    case 'mergeCells': c.mergeCells().run(); return true;
    case 'splitCell': c.splitCell().run(); return true;
    case 'deleteTable': c.deleteTable().run(); return true;
    default: return false;
  }
}

/**
 * 光标所在的代码块。
 *
 * 代码块在 ProseMirror 里是 textblock，`$from` 往上走就能找到它；节点位置用
 * `$from.before(depth)`（**节点之前**的位置），`updateAttributes` 认这个位置。
 *
 * @returns {null | {pos: number, dom: HTMLElement|null, lang: string}}
 *   不在代码块里返回 null。
 */
function currentCodeBlock(editor) {
  const $from = editor.state.selection.$from;
  for (let d = $from.depth; d > 0; d--) {
    const node = $from.node(d);
    if (node.type.name !== 'codeBlock') continue;
    const pos = $from.before(d);
    return { pos, dom: codeBlockDom(editor, pos), lang: node.attrs.language || '' };
  }
  return null;
}

/** 代码块节点对应的 DOM（`<pre>`）；找不到返回 null。 */
function codeBlockDom(editor, pos) {
  let dom = null;
  try {
    dom = editor.view.nodeDOM(pos);
  } catch (e) {
    dom = null;
  }
  if (dom && dom.nodeType === 1) {
    const pre = dom.closest ? dom.closest('pre') : null;
    if (pre) return pre;
  }
  // 退路：按位置取 DOM，再往上找最近的 <pre>。
  const at = editor.view.domAtPos(pos + 1);
  let n = at && at.node;
  if (n && n.nodeType !== 1) n = n.parentElement;
  return n && n.closest ? n.closest('pre') : null;
}

/**
 * 代码块语言下拉。
 *
 * **不是工具栏上的固定控件**，而是浮在光标所在代码块的右上角 —— 工具栏上的那个
 * 控件有两个毛病：一是跟代码块对不上号（有好几个代码块时不知道改哪个），二是
 * 常年占着工具栏的位置。
 *
 * 原生 `<select>` 的下拉**靠 mousedown 的默认行为弹出**，所以这里绝对不能
 * `preventDefault()` —— 拦掉它下拉就打不开（工具栏上那个旧控件就是这么坏的）。
 * 点开下拉会让编辑器失焦，但 ProseMirror 的 `state.selection` 不受影响，
 * 改语言时照样能定位到代码块。
 */
function createLangPicker(editor) {
  const sel = document.createElement('select');
  sel.className = 'tiptap-cb-lang';
  sel.title = '代码块语言';
  sel.hidden = true;
  for (const [value, label] of CODE_LANGUAGES) {
    const opt = document.createElement('option');
    opt.value = value;
    opt.textContent = label;
    sel.appendChild(opt);
  }
  sel.addEventListener('change', () => {
    const lang = sel.value;
    const cb = currentCodeBlock(editor);
    if (!cb) return;
    editor.chain().focus().updateAttributes('codeBlock', { language: lang || null }).run();
  });
  return sel;
}

/** 语言不在清单里（存量正文可能是 lowlight 的其它语言）时补一项，免得下拉显示成空白。 */
function ensureLangOption(sel, lang) {
  if (!lang) return;
  for (const o of sel.options) {
    if (o.value === lang) return;
  }
  const opt = document.createElement('option');
  opt.value = lang;
  opt.textContent = lang;
  sel.appendChild(opt);
}

/**
 * 把语言下拉贴到代码块右上角。
 *
 * 定位用视口坐标（`position: fixed`）：编辑器容器是 Leptos 渲染的，在它身上加类
 * 会被重渲染抹掉，绝对定位的参照系不稳。
 */
function positionLangPicker(sel, cb, lang) {
  if (!cb || !cb.dom) {
    sel.hidden = true;
    return;
  }
  sel.hidden = false;
  ensureLangOption(sel, lang);
  if (sel.value !== lang) sel.value = lang;
  const rect = cb.dom.getBoundingClientRect();
  const w = sel.offsetWidth;
  sel.style.left = `${Math.max(8, Math.round(rect.right - w - 8))}px`;
  sel.style.top = `${Math.round(rect.top + 6)}px`;
}

/**
 * 建工具栏。
 *
 * @param {object} editor Tiptap Editor 实例
 * @param {object} opts { upload: (file) => Promise<string>, onImagePick }
 * @returns {HTMLElement} 工具栏 DOM（调用方负责插到编辑器容器之前）
 */
export function createToolbar(editor, opts = {}) {
  const bar = document.createElement('div');
  bar.className = 'tiptap-toolbar';
  bar.setAttribute('role', 'toolbar');

  const buttons = new Map();

  for (const group of GROUPS) {
    const g = document.createElement('div');
    g.className = 'tiptap-tb-group';
    for (const [cmd, title, icon, isToggle] of group) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'tiptap-tb-btn';
      btn.dataset.cmd = cmd;
      btn.title = title;
      btn.setAttribute('aria-label', title);
      if (cmd === 'code') btn.classList.add('tiptap-tb-code');
      if (cmd === 'italic') btn.classList.add('tiptap-tb-italic');
      if (cmd === 'underline') btn.classList.add('tiptap-tb-underline');
      if (cmd === 'strike') btn.classList.add('tiptap-tb-strike');
      btn.textContent = icon;
      btn.addEventListener('mousedown', (e) => e.preventDefault()); // 别让按钮抢走选区
      btn.addEventListener('click', (e) => {
        e.preventDefault();
        handleClick(editor, cmd, btn, opts);
      });
      g.appendChild(btn);
      buttons.set(cmd, { btn, isToggle });
    }
    bar.appendChild(g);
  }

  // 代码块语言选择：浮在光标所在代码块的右上角（不在工具栏上）
  const langSel = createLangPicker(editor);

  // 表格浮动条：光标在表格内时浮在表格上方
  const tableBar = document.createElement('div');
  tableBar.className = 'tiptap-tablebar';
  tableBar.hidden = true;
  for (const [cmd, title, icon] of TABLE_ACTIONS) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'tiptap-tb-btn';
    if (cmd.startsWith('delete')) btn.classList.add('tiptap-tb-danger');
    btn.title = title;
    btn.setAttribute('aria-label', title);
    // 内联 SVG 直接当 HTML 塞进去；其余仍是纯文字符号。
    if (icon.trimStart().startsWith('<')) btn.innerHTML = icon;
    else btn.textContent = icon;
    btn.addEventListener('mousedown', (e) => e.preventDefault());
    btn.addEventListener('click', (e) => {
      e.preventDefault();
      runCommand(editor, cmd);
    });
    tableBar.appendChild(btn);
  }

  // 激活态跟随编辑器状态
  const refresh = () => {
    for (const [cmd, { btn, isToggle }] of buttons) {
      if (!isToggle) continue;
      const active = isActive(editor, cmd);
      btn.classList.toggle('is-active', active);
    }
    const inTable = editor.isActive('table');
    tableBar.hidden = !inTable;
    if (inTable) {
      positionTableBar(editor, tableBar);
    }
    // 代码块语言：跟着光标所在的代码块走
    const cb = currentCodeBlock(editor);
    if (cb) {
      positionLangPicker(langSel, cb, cb.lang);
    } else {
      langSel.hidden = true;
    }
  };
  editor.on('selectionUpdate', refresh);
  editor.on('transaction', refresh);
  refresh();

  // 滚动时把浮动控件拉回代码块（fixed 定位不跟着文档走）。
  const reposition = () => {
    const cb = currentCodeBlock(editor);
    if (cb) positionLangPicker(langSel, cb, cb.lang);
  };
  window.addEventListener('scroll', reposition, true);
  window.addEventListener('resize', reposition);

  return { bar, tableBar, langSel, refresh };
}

/** 把表格浮动条放到当前表格的上方（相对编辑器容器定位）。 */
function positionTableBar(editor, tableBar) {
  const sel = editor.state.selection;
  const domAt = editor.view.domAtPos(sel.from);
  const node = domAt && domAt.node;
  const tableEl = node && node.nodeType === 1 ? node.closest('table') : null;
  if (!tableEl) return;
  // fixed 定位：用视口坐标，不依赖定位祖先（编辑器容器是 Leptos 渲染的，
  // 在它身上加类会被重渲染抹掉）。
  const tr = tableEl.getBoundingClientRect();
  const content = editor.view.dom.getBoundingClientRect();
  let top = tr.top - tableBar.offsetHeight - 4;
  // 表格紧贴正文区顶部时，上方放不下 —— 宁可压住表格首行，也别盖住工具栏：
  // 工具栏上的按钮点不到，比表格少看一行严重得多。
  if (top < content.top) top = content.top + 2;
  tableBar.style.left = `${tr.left}px`;
  tableBar.style.top = `${top}px`;
}

/** cmd 的激活态判定。 */
function isActive(editor, cmd) {
  switch (cmd) {
    case 'h1': return editor.isActive('heading', { level: 1 });
    case 'h2': return editor.isActive('heading', { level: 2 });
    case 'h3': return editor.isActive('heading', { level: 3 });
    default: return editor.isActive(cmd);
  }
}

/** 点按钮的实际动作（有一部分不是纯 chain 命令）。 */
function handleClick(editor, cmd, btn, opts) {
  switch (cmd) {
    case 'link': {
      const prev = editor.getAttributes('link').href || '';
      const url = window.prompt('链接地址（留空移除链接）', prev);
      if (url === null) return;
      if (!url.trim()) {
        editor.chain().focus().unsetLink().run();
        return;
      }
      editor.chain().focus().extendMarkRange('link').setLink({ href: url.trim() }).run();
      return;
    }
    case 'image': {
      const input = document.createElement('input');
      input.type = 'file';
      input.accept = 'image/*';
      input.addEventListener('change', async () => {
        const file = input.files && input.files[0];
        if (!file) return;
        try {
          const url = await opts.upload(file);
          editor.chain().focus().setImage({ src: url }).run();
        } catch (err) {
          window.console.warn('图片上传失败', err);
        }
      });
      input.click();
      return;
    }
    case 'table': {
      editor.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run();
      return;
    }
    default:
      runCommand(editor, cmd);
  }
}
