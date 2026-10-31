// 代码块里的编辑增强：括号自动补充、Enter 自动缩进、Tab = 4 空格。
//
// 只在 codeBlock 节点里生效。正文（非列表 / 非表格）里的 Tab 仍然插制表符，
// 见 glue.js 的 handleTabKey。
//
// 三个增强都挂在编辑器已有的钩子上：
//   · 括号补充 → handleTextInput（拿得到"刚打进去的那个字符"）
//   · 自动缩进 → handleKeyDown（Enter 不走文本输入路径）
//   · Tab      → handleKeyDown

const INDENT = '    '; // 4 空格

// 开括号 → 闭括号
const PAIRS = { '(': ')', '[': ']', '{': '}' };
const CLOSERS = ')]}';

/**
 * 光标所在的代码块上下文。
 *
 * 代码块的 content 是 "text*"（一整块文本，\n 直接存在文本里），所以块内全文
 * 就是 parent.textContent，光标在块内的偏移是 parentOffset。
 */
function blockContext(state) {
  const { $from, $to } = state.selection;
  if ($from.parent.type.name !== 'codeBlock') return null;
  if ($to.parent.type.name !== 'codeBlock') return null; // 选区跨出了代码块
  return {
    text: $from.parent.textContent,
    base: $from.start(), // 块内第 0 个字符对应的文档位置
    offset: $from.parentOffset,
  };
}

/** 当前行（光标所在行）的行首空白。 */
function currentIndent(text, offset) {
  const lineStart = text.lastIndexOf('\n', offset - 1) + 1;
  const line = text.slice(lineStart, offset);
  const m = /^[ \t]*/.exec(line);
  return { indent: m ? m[0] : '', line };
}

/**
 * 括号自动补充。
 *
 * · 打 `(` / `[` / `{` → 补上对应的闭括号，光标停在中间。
 *   选中了一段文本时改成"用括号把选区包起来"。
 * · 打 `)` / `]` / `}` 而光标右边已经是同款闭括号 → 直接跳过它，不重复插入。
 */
export function codeBlockTextInput(editor, view, from, to, text) {
  if (!editor || text.length !== 1) return false;
  const ctx = blockContext(view.state);
  if (!ctx) return false;

  const localFrom = from - ctx.base;
  const localTo = to - ctx.base;
  const selected = ctx.text.slice(localFrom, localTo);

  const closer = PAIRS[text];
  if (closer) {
    if (selected) {
      editor
        .chain()
        .command(({ tr }) => {
          tr.insertText(text + selected + closer, from, to);
          return true;
        })
        .setTextSelection({ from: from + 1, to: to + 1 })
        .run();
    } else {
      editor
        .chain()
        .command(({ tr }) => {
          tr.insertText(text + closer, from, to);
          return true;
        })
        .setTextSelection(from + 1)
        .run();
    }
    return true;
  }

  if (CLOSERS.includes(text)) {
    const right = ctx.text.slice(localTo, localTo + 1);
    if (right === text) {
      // 光标右边已经有一个了 —— 跳过，不重复插
      editor.chain().setTextSelection(to + 1).run();
      return true;
    }
  }
  return false;
}

/**
 * Enter：自动缩进。
 *
 * · 当前行有内容 → 换行后保持同样的缩进；行尾是开括号时再多缩进一级。
 * · 光标右边紧跟闭括号 → 把它挪到下一行（{ ⏎ } 那种三行展开）。
 * · 当前行是空行且光标在块末尾 → 退出代码块（留一个出口，不然进得去出不来）。
 */
export function codeBlockEnter(editor, view) {
  if (!editor) return false;
  const ctx = blockContext(view.state);
  if (!ctx) return false;
  const { text, base, offset } = ctx;
  const { from, to } = view.state.selection;

  const { indent, line } = currentIndent(text, offset);
  const blank = line.trim() === '';
  const atEnd = offset === text.length;

  if (blank && atEnd) {
    // 空行上回车 → 退出代码块（先把这行的空白删掉）
    editor
      .chain()
      .command(({ tr }) => {
        tr.delete(base + text.lastIndexOf('\n', offset - 1) + 1, base + offset);
        return true;
      })
      .exitCode()
      .run();
    return true;
  }

  const trimmed = line.replace(/\s+$/, '');
  const opener = trimmed.length ? trimmed[trimmed.length - 1] : '';
  const opensBlock = Object.prototype.hasOwnProperty.call(PAIRS, opener);
  const nextIndent = indent + (opensBlock ? INDENT : '');
  const right = text.slice(offset, offset + 1);
  const split = opensBlock && right === PAIRS[opener];

  const insert = split ? '\n' + nextIndent + '\n' + indent : '\n' + nextIndent;
  editor
    .chain()
    .command(({ tr }) => {
      tr.insertText(insert, from, to);
      return true;
    })
    .setTextSelection(from + 1 + nextIndent.length)
    .run();
  return true;
}

/**
 * Tab：4 空格。
 *
 * 光标空 → 直接插 4 个空格；有选区 → 给选区覆盖的每一行行首加 4 空格
 * （从后往前插，前面的位置才不会因为插字而偏移）。
 */
export function codeBlockTab(editor, view) {
  if (!editor) return false;
  const ctx = blockContext(view.state);
  if (!ctx) return false;
  const { text, base } = ctx;
  const { from, to, empty } = view.state.selection;
  const localFrom = from - base;
  const localTo = to - base;

  if (empty) {
    editor
      .chain()
      .command(({ tr }) => {
        tr.insertText(INDENT);
        return true;
      })
      .run();
    return true;
  }

  // 选区覆盖到的每一行的行首
  const starts = [];
  let lineStart = 0;
  for (let i = 0; i <= text.length; i++) {
    if (i === text.length || text[i] === '\n') {
      if (lineStart <= localTo && i >= localFrom) starts.push(base + lineStart);
      lineStart = i + 1;
      if (i === text.length) break;
    }
  }
  if (!starts.length) return false;

  editor
    .chain()
    .command(({ tr }) => {
      for (let i = starts.length - 1; i >= 0; i--) tr.insertText(INDENT, starts[i]);
      return true;
    })
    .run();
  return true;
}
