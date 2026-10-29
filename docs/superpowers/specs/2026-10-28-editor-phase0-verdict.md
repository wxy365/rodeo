# 阶段 0 闸口评估结论：编辑器表格与语法高亮

日期：2026-10-28（结论 2026-10-29 修订）
结论（修订后）：**推进 Tiptap v3 迁移**。阶段 0 的成果保留为迁移的基线与验收清单。

对应设计稿：`2026-10-28-editor-tiptap-migration-design.md` §3.5 的判据。

---

## 1. 交付了什么

| 能力 | 状态 | 实现方式 |
|---|---|---|
| 工具栏插入表格 | ✅ | `better-table` 模块**今天就已经在跑**（themes/snow 的 DEFAULTS.modules 里带了它），只差工具栏一行 `['better-table']` |
| 表格 3×3 插入 / 行列选择器 / 单元格内换行 | ✅ | 模块自带 |
| 表格右键菜单（合并单元格 / 拆分 / 删行列 / 插入行列） | ✅ | 模块自带（**文案是硬编码英文**） |
| 表格内容存 / 取 | ✅ | Delta 落库，刷新后还原 |
| 代码块语言下拉 | ✅ | `modules/syntax` + vendor 的 highlight.js 11.12.0 |
| 代码块语法高亮 | ✅ | 同上（lowlight 不需要，Quill 的 syntax 模块直接用 hljs） |
| 代码块存 / 取 | ✅ | 同上 |
| 粘贴 markdown 表格 → 原生表格 | ✅ | 纯文本与 HTML 两条剪贴板分支都走 clipboard matcher 转成模块自己的表格 |
| 老评论（纯文本）只读渲染 | ✅ | 顺带修掉「正文空白」的缺陷 |
| AI 对话面板渲染 markdown 表格 | ✅ | `agent_panel.rs` 的 `render_markdown` 加表格分支 |

**没有换编辑器，没有改内容存储格式**（仍然是 Quill Delta），`data/` 目录的
bincode 结构一个字没动 —— 阶段 0 完全可回滚。

## 2. 验证结果

浏览器验证脚本：`scripts/editor-smoke.mjs`（playwright-core 走 `target/site-smoke`
的私有 site root，不碰用户的 `:3000`）。**10 个场景全过**：

| 场景 | 覆盖 | 结果 |
|---|---|---|
| `hljs-loaded` | hljs 就位、script 顺序在 glue.js 之前、无 JS 错误 | ✅ |
| `syntax-basic` | 代码块语言下拉出现、含 rust/javascript、选语言后高亮 token 出现 | ✅ |
| `syntax-persist` | 编辑 → 保存 → 刷新，正文还在 | ✅ |
| `table-basic` | 表格按钮、3×3 插入、单元格打字、右键菜单、存/取 | ✅ |
| `table-extra` | hover 行列选择器、单元格内换行、往单元格里粘贴不插新表 | ✅ |
| `table-paste` | 纯文本 markdown 表格 → 真表格（3 行 2 列，表头行内 `<strong>`） | ✅ |
| `table-paste-html` | HTML 剪贴板：正文保持粗体，表格那段变成真表格 | ✅ |
| `md-table-readonly` | 老评论（纯文本 + markdown 表格）只读渲染出表格、正文不再空白、视图里没有语言下拉 | ✅ |
| `editor-regression` | @提及下拉、标题、链接按钮、勾选列表 | ✅ |
| `image-paste` | 粘贴图片 → 走 uploadAttachment 上传 → 插进编辑器；上传后立刻保存不报冲突 | ✅ |

Rust 侧的表格识别逻辑（`agent_panel.rs` 的 `split_md_row` / `parse_md_table_at`）
用 `rustc` 单独编译跑了 22 项断言（与 JS 侧用例一一对应），全过。

## 3. 未覆盖 / 需人工验证

| 项 | 原因 |
|---|---|
| Safari 拼音在**代码块**内输入 | headless Chromium 不覆盖 IME，需人工在 Safari 26.2 + 系统拼音下验证。列表项的同类问题历史上已修（`glue.js` 的 `attachUI` 补丁），代码块是**新路径**（syntax 的 `<select>` 也是同类结构） |
| better-table 右键菜单的「背景色」 | 断言只覆盖了合并/拆分/删行列；「背景色」在主题默认的 `operationMenu:{color:true}` 里，实测菜单里有对应项但未单独断言 |
| AI 对话面板的 markdown 表格（端到端） | 需要模型真的输出一张表。本轮只验证了 Rust 侧解析与渲染逻辑 |
| —— | （原缺口「图片粘贴上传回归」已由 `image-paste` 场景补上） |

## 4. 闸口判据对照

设计稿 §3.5 的「取消迁移」条件：表格能插/能改行列/能合并单元格/能存能取/
只读渲染正确；代码块语言下拉可用/高亮生效/能存能取；图片上传、@提及、
勾选列表无回归。

| 条件 | 结果 |
|---|---|
| 表格：能插 | ✅ |
| 表格：能改行列 | ✅（右键菜单 + hover 行列选择器） |
| 表格：能合并单元格 | ✅ |
| 表格：能存能取 | ✅ |
| 表格：只读渲染正确 | ✅ |
| 代码块：语言下拉可用 | ✅ |
| 代码块：高亮生效 | ✅ |
| 代码块：能存能取 | ✅ |
| 图片上传无回归 | ✅（`image-paste` 场景：粘贴 → 上传 → 插图 → 紧接着保存不报冲突） |
| @提及无回归 | ✅ |
| 勾选列表无回归 | ✅（注：fluent-editor 的 checked 列表**没有工具栏按钮**，本次也没加） |
| 中文输入法在列表/表格/代码块都不丢字 | ⚠️ 需人工在 Safari 上验证 |

**进入阶段 1 的触发条件**（任一）：表格缺列宽拖拽等结构性能力且短期补不上；
代码块高亮在 Safari 上有问题；`glue.js` 补丁层继续失控；需要编辑器级可扩展性。

本轮实测：**这四条都没触发**。表格的行列选择器、单元格换行、合并单元格都是
模块自带的；语法高亮模块也是现成的。`glue.js` 从 566 行涨到 ~900 行，其中
新增的 ~350 行是 markdown 表格识别与粘贴转换（这一块 Quill 没有，得自己写），
不是「打补丁救火」。

## 5. 结论（2026-10-29 修订）

**原结论是「取消阶段 1」，现推翻，改为推进 Tiptap v3 迁移。**

推翻的理由不是阶段 0 有功能缺口 —— 表格、语法高亮代码块、markdown 表格粘贴、
老评论只读渲染都到位了，10 个场景全过。**理由是界面层**：fluent-editor 给的是
「Quill snow 主题 + 一排图标」，而「完善的编辑器」指的是现代工具栏、slash 菜单、
拖拽块、浮动格式菜单、真表格、代码块语言选择、@提及浮层 —— 这些要么没有，
要么改不动（`.ql-*` 那 98 处 CSS 覆写就是「改不动」的证据）。

### 阶段 0 的成果不丢弃，它变成迁移的基线

| 成果 | 在迁移里的角色 |
|---|---|
| markdown 表格识别规则（`glue.js` 的 `splitMdRow`/`parseMdTableAt` 与 `agent_panel.rs` 的同名函数，两边各一份） | **直接复用**，Tiptap 侧照抄 JS 那份即可 |
| 存量 Delta 的真实形状摸清了（`table-col` / `table-cell-line` / `code-token` / `code-block: "<语言>"`） | 转换清单不会再漏 —— 漏一个就是静默丢内容 |
| `scripts/editor-smoke.mjs` 的 10 个场景 | **直接当迁移的验收清单** |
| better-table / syntax 模块的行为（右键菜单、代码块高亮、语言表扩展到 23 项） | 作为「功能对标清单」，Tiptap 侧逐条验收 |
| `public/tiny-editor/hljs.min.js` 的 vendor 方式（CDN 抓 UMD 产物直接 commit） | Tiptap 侧改用 esbuild 打包（见阶段 1 Task 1） |

### 阶段 1 的完整计划

`docs/superpowers/plans/2026-10-28-editor-tiptap-migration.md`（14 个任务）。

**执行顺序按计划自检里的结论调整**：存储格式（Task 3/4/5）必须排在编辑器替换
（Task 2）之前，否则中间态没法验证。

### 留下的尾巴

0. **页面级脚本别放 App 视图**。给 hljs 加载加一行 leptos_meta 的注册式 `<Script>`，
   hydration 直接崩 —— 而 SSR 出来的页面看着完全正常，只是前端不接管任何交互，
   且 `page.on('pageerror')` 抓不到（走 console.error）。最终改放 shell 的 `<head>`
   里用普通 `<script>`。详见设计稿 §3.2.1。
1. **better-table 的右键菜单是英文**（`Merge Cells` / `Split Cells` /
   `Delete Entire Row` …），fluent-editor 没给它做 i18n，而 rodeo 界面是中文的。
   模块没暴露 label 选项，要中文化只能改 vendor bundle。**优先级低，但在那里。**
2. **代码块的语言清单**原本只有 14 项（没有 Rust/Go/TypeScript/JSON/YAML），
   本轮扩到 23 项（key 都是 highlight.js 认识的名字或别名）。
3. **syntax 模块会把高亮结果写回模型**（带 `code-token` 属性的 Delta），
   于是 `code-block` 的落库取值是**语言字符串**（`"plain"` / `"rust"`）而不是 `true`，
   且文本带 `code-token` 属性。将来若真做阶段 1，`delta_to_html` 的转换清单
   **必须包含 `code-token` / `table-col` / `table-cell-line`**，漏一个就是静默丢内容。
