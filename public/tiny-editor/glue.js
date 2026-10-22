// Rodeo tiny-editor 桥接脚本（ES module）。
// 由 app shell 以 <script type="module" src="/tiny-editor/glue.js"> 加载。
// 暴露 window.__rodeo_tiny_editor__.create(el, deltaJson) 供 Rust/WASM 侧调用。
//
// 升级方式：npm 包名实为 @opentiny/fluent-editor（Quill 2.0 版）。
//   - fluent-editor.mjs ← https://esm.sh/@opentiny/fluent-editor@<ver>/es2022/fluent-editor.bundle.mjs
//   - style.css         ← https://unpkg.com/@opentiny/fluent-editor@<ver>/style.css
import FluentEditor from './fluent-editor.mjs';

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

// 输入法组合文本兜底（Safari / 部分中文 IME）。
//
// 两个已知的丢字路径，都会让「DOM 里有中文、Quill 的模型里没有」：
//   1) 组合文本被塞进 `<span class="ql-ui" contenteditable="false">` 这个
//      标记节点（有序/无序列表每项的第一个子节点）。它不是一个 blot，
//      Quill 的 mutation diff 完全看不见它，下一次按模型重写 DOM 就没了。
//   2) 浏览器在组合输入期间丢 MutationRecord（slab/quill#3837、#3969）：
//      文本节点已经插进 li，但 Quill 的 MutationObserver 没收到记录，
//      blot 树/Delta 里没有这段文字，下一次 update 就会把它删掉。
//
// compositionend 后统一修：
//   - 把 ql-ui 里的孤儿文本挪回 li；
//   - 扫一遍编辑器的文本节点：未注册成 blot 的、以及 text blot 的 text
//     与 DOM 不一致的，合成 MutationRecord 交给 scroll.update()——让
//     Quill 走它本来该走的 mutation 处理路径，把文字收进模型。
// 正常输入时两者都无事可做（DOM 与模型一致），不会产生额外改动。
function repairCompositionText(el, editor) {
  // 每个容器只绑一次；节点被复用、create 再来一遍时不叠加监听。
  if (el.__rodeo_composition_repair) return;
  el.__rodeo_composition_repair = true;
  el.addEventListener('compositionend', () => {
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
        modules: { toolbar: false },
      });
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
        modules: { toolbar: false },
      });
    }
    try {
      const parsed = normalizeListAttributes(JSON.parse(deltaJson));
      renderEditor.setContents(parsed, 'silent');
      // 导出窗口：只在这段同步调用里让 ListBlot.formats() 返回字符串，
      // 导出器才能把相邻列表项归进同一个 <ol>/<ul>。
      exportingSemantic = true;
      try {
        return renderEditor.getSemanticHTML();
      } finally {
        exportingSemantic = false;
      }
    } catch (e) {
      return '';
    }
  },
};
