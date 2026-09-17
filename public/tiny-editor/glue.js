// Rodeo tiny-editor 桥接脚本（ES module）。
// 由 app shell 以 <script type="module" src="/tiny-editor/glue.js"> 加载。
// 暴露 window.__rodeo_tiny_editor__.create(el, deltaJson) 供 Rust/WASM 侧调用。
//
// 升级方式：npm 包名实为 @opentiny/fluent-editor（Quill 2.0 版）。
//   - fluent-editor.mjs ← https://esm.sh/@opentiny/fluent-editor@<ver>/es2022/fluent-editor.bundle.mjs
//   - style.css         ← https://unpkg.com/@opentiny/fluent-editor@<ver>/style.css
import FluentEditor from './fluent-editor.mjs';

// 只清理本容器上一次留下的工具栏/浮层。Quill 的 snow 主题把工具栏插成容器的
// 前一个兄弟节点，容器卸载时不会被一并带走；早先这里按类名全局清理，但页面上
// 同时存在详情编辑器与评论编辑器之后，全局清理会抹掉另一个编辑器的工具栏。
function cleanupAux(el) {
  if (el.__rodeo_aux) {
    el.__rodeo_aux.forEach((n) => n.remove());
    el.__rodeo_aux = [];
  }
  // 浮层（ql-tooltip）可能被挂到 body 上，只扫 body 的直接子节点，
  // 不会碰到其他编辑器容器内部的东西。
  document.querySelectorAll('body > .ql-tooltip').forEach((n) => n.remove());
}

// 正文不支持图片：Quill 默认把粘贴/拖入的图片转成 base64 embed，单条评论能到数 MB，
// 且只读渲染会把 `data:` URI 原样输出。在编辑器自身的输入入口拦掉，工具栏本来就没有图片按钮。
function blockImages(el) {
  const hasImage = (dt) => {
    const items = dt && dt.items;
    if (!items) return false;
    for (const it of Array.from(items)) {
      if (it.kind === 'file' && (it.type || '').startsWith('image/')) return true;
    }
    return false;
  };
  const guard = (e) => {
    if (hasImage(e.clipboardData || e.dataTransfer)) {
      e.preventDefault();
      e.stopPropagation();
    }
  };
  // 捕获阶段：Quill 的 clipboard 模块在冒泡阶段处理粘贴，这里要抢在它之前。
  el.addEventListener('paste', guard, true);
  el.addEventListener('drop', guard, true);
}

// 只读渲染复用同一个离屏实例；整页评论共用一个，与评论条数无关。
let renderHost = null;
let renderEditor = null;

window.__rodeo_tiny_editor__ = {
  // 创建编辑器实例。deltaJson 为已归一化的 Quill Delta JSON 字符串（可为空串）。
  create(el, deltaJson) {
    cleanupAux(el);
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
    blockImages(el);

    if (deltaJson) {
      try {
        editor.setContents(JSON.parse(deltaJson), 'silent');
      } catch (e) {
        // 非法 Delta 时忽略，编辑器保持空内容。
      }
    }

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
