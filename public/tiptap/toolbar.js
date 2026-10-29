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

/** 表格浮动条的操作。[cmd, 标题, 图标字符] */
const TABLE_ACTIONS = [
  ['addRowAfter', '下方插入行', '⤓'],
  ['deleteRow', '删除本行', '⤒'],
  ['addColumnAfter', '右侧插入列', '⇥'],
  ['deleteColumn', '删除本列', '⇤'],
  ['mergeCells', '合并单元格', '⿴'],
  ['splitCell', '拆分单元格', '⿲'],
  ['deleteTable', '删除表格', '✕'],
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

  // 代码块语言选择：作用于光标所在的代码块
  const langSel = document.createElement('select');
  langSel.className = 'tiptap-tb-lang';
  langSel.title = '代码块语言';
  for (const [value, label] of CODE_LANGUAGES) {
    const opt = document.createElement('option');
    opt.value = value;
    opt.textContent = label;
    langSel.appendChild(opt);
  }
  langSel.addEventListener('mousedown', (e) => e.preventDefault());
  langSel.addEventListener('change', () => {
    const lang = langSel.value;
    editor.chain().focus().updateAttributes('codeBlock', { language: lang }).run();
  });
  bar.appendChild(langSel);

  // 表格浮动条：光标在表格内时浮在表格上方
  const tableBar = document.createElement('div');
  tableBar.className = 'tiptap-tablebar';
  tableBar.hidden = true;
  for (const [cmd, title, icon] of TABLE_ACTIONS) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'tiptap-tb-btn';
    btn.title = title;
    btn.setAttribute('aria-label', title);
    btn.textContent = icon;
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
    // 代码块语言
    const lang = editor.isActive('codeBlock') ? editor.getAttributes('codeBlock').language : null;
    if (lang != null) langSel.value = lang;
  };
  editor.on('selectionUpdate', refresh);
  editor.on('transaction', refresh);
  refresh();

  return { bar, tableBar, refresh };
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
  tableBar.style.left = `${tr.left}px`;
  tableBar.style.top = `${tr.top - tableBar.offsetHeight - 4}px`;
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
