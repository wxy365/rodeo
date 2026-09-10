// Rodeo tiny-editor 桥接脚本（ES module）。
// 由 app shell 以 <script type="module" src="/tiny-editor/glue.js"> 加载。
// 暴露 window.__rodeo_tiny_editor__.create(el, deltaJson) 供 Rust/WASM 侧调用。
//
// 升级方式：npm 包名实为 @opentiny/fluent-editor（Quill 2.0 版）。
//   - fluent-editor.mjs ← https://esm.sh/@opentiny/fluent-editor@<ver>/es2022/fluent-editor.bundle.mjs
//   - style.css         ← https://unpkg.com/@opentiny/fluent-editor@<ver>/style.css
import FluentEditor from './fluent-editor.mjs';

window.__rodeo_tiny_editor__ = {
  // 创建编辑器实例。deltaJson 为已归一化的 Quill Delta JSON 字符串（可为空串）。
  create(el, deltaJson) {
    // 清理上一次编辑器残留的工具栏/浮层。工具栏可能被渲染为容器外的兄弟节点，
    // 随容器卸载不会被一并移除，因此这里按类名全局清一遍（本应用同一时刻只有一个编辑器）。
    document.querySelectorAll('.ql-toolbar, .ql-tooltip').forEach((n) => n.remove());
    // 再清空容器，避免复用节点时叠加旧内容。
    el.innerHTML = '';
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

    if (deltaJson) {
      try {
        editor.setContents(JSON.parse(deltaJson), 'silent');
      } catch (e) {
        // 非法 Delta 时忽略，编辑器保持空内容。
      }
    }

    return editor;
  },
};
