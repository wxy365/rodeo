// 把 Tiptap 打成一个自包含的 ESM：public/tiptap/editor.mjs。
//
// 产物 **vendor 进仓库**（和 public/tiny-editor/fluent-editor.mjs 一个做法）：
// 私有化部署是离线环境，运行时不能依赖 CDN，也不能依赖构建机上有 node。
// 升级 Tiptap 时改 package.json 的版本号，然后跑 `make editor` 重新生成。
//
// 依赖版本在 package.json 里锁死（不用 ^）：产物进仓库，浮动版本会让
// 「重新生成产物」变成不可复现的操作。
import { build } from 'esbuild';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const outfile = path.join(root, 'public/tiptap/editor.mjs');

const result = await build({
  entryPoints: [path.join(root, 'scripts/editor-entry.mjs')],
  bundle: true,
  format: 'esm',
  target: ['es2022'],
  minify: true,
  outfile,
  legalComments: 'none',
  // lowlight / prosemirror 里有 process.env.NODE_ENV 的分支
  define: { 'process.env.NODE_ENV': '"production"' },
  metafile: true,
  logLevel: 'warning',
});

const bytes = Object.values(result.metafile.outputs)[0]?.bytes ?? 0;
console.log(`public/tiptap/editor.mjs  ${(bytes / 1024).toFixed(1)} KB`);
