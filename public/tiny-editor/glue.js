// Rodeo tiny-editor 桥接脚本（ES module）。
// 由 app shell 以 <script type="module" src="/tiny-editor/glue.js"> 加载。
// 暴露 window.__rodeo_tiny_editor__.create(el, deltaJson) 供 Rust/WASM 侧调用。
//
// 升级方式：npm 包名实为 @opentiny/fluent-editor（Quill 2.0 版）。
//   - fluent-editor.mjs ← https://esm.sh/@opentiny/fluent-editor@<ver>/es2022/fluent-editor.bundle.mjs
//   - style.css         ← https://unpkg.com/@opentiny/fluent-editor@<ver>/style.css
import FluentEditor from './fluent-editor.mjs';

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
  // 浮层（ql-tooltip）可能被挂到 body 上，只扫 body 的直接子节点，
  // 不会碰到其他编辑器容器内部的东西。
  document.querySelectorAll('body > .ql-tooltip').forEach((n) => n.remove());
}

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
          readOnlyEditor.setContents(JSON.parse(deltaJson), 'silent');
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

    if (deltaJson) {
      try {
        editor.setContents(JSON.parse(deltaJson), 'silent');
      } catch (e) {
        // 非法 Delta 时忽略，编辑器保持空内容。
      }
    }

    // 同一次响应式更新里可能还有别处的编辑器被卸载，卸载与新建的先后顺序
    // 由 Leptos 决定。等这次更新彻底落定后再扫一遍，漏网的孤儿也一并清掉。
    setTimeout(sweepOrphans, 0);

    return editor;
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
      renderEditor.setContents(JSON.parse(deltaJson), 'silent');
      return renderEditor.getSemanticHTML();
    } catch (e) {
      return '';
    }
  },
};
