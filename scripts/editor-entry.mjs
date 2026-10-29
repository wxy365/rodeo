// Tiptap 打包入口：把编辑器要用到的扩展从这里统一导出，esbuild 打成一个 ESM。
//
// 为什么要单独一个入口文件：esbuild 的 bundle 入口必须是一个模块，
// 而 public/tiptap/glue.js 是运行时代码（要能被浏览器直接 import），
// 两者不能是同一个文件。
export { Editor } from '@tiptap/core';
export { default as StarterKit } from '@tiptap/starter-kit';
// extension-table 没有 default 导出，TableKit 是具名导出。
export { TableKit } from '@tiptap/extension-table';
export { default as Image } from '@tiptap/extension-image';
export { default as Link } from '@tiptap/extension-link';
export { default as Mention } from '@tiptap/extension-mention';
export { default as Placeholder } from '@tiptap/extension-placeholder';
export { default as TaskList } from '@tiptap/extension-task-list';
export { default as TaskItem } from '@tiptap/extension-task-item';
export { default as CodeBlockLowlight } from '@tiptap/extension-code-block-lowlight';
export { default as Underline } from '@tiptap/extension-underline';
export { default as TextAlign } from '@tiptap/extension-text-align';
export { default as Highlight } from '@tiptap/extension-highlight';
export { common, createLowlight } from 'lowlight';
