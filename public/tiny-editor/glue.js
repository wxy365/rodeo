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
  create(el, deltaJson, opts) {
    cleanupAux(el);
    sweepOrphans();
    // 再清空容器，避免复用节点时叠加旧内容。
    el.innerHTML = '';
    // 记下这次新建过程中新增的兄弟节点（工具栏/浮层），供下次创建时精确清理。
    const before = new Set(el.parentNode.children);
    const editor = new FluentEditor(el, {
      theme: 'snow',
      placeholder: '输入详情…',
      modules: {
        toolbar: [
          ['bold', 'italic', 'underline', 'strike'],
          [{ header: 1 }, { header: 2 }, { header: 3 }],
          [{ list: 'ordered' }, { list: 'bullet' }],
          ['blockquote', 'code-block'],
          ['link'],
          ['clean'],
        ],
      },
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
