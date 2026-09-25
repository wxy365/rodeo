# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目一句话

Rodeo 是一个用 Rust 写的全栈任务/问题跟踪 Web 应用：Leptos 0.8 同时出 SSR + WASM hydrate，Axum 0.8 暴露 GraphQL，DocStore 走 RocksDB（可选 Postgres），附件走本地 FS（可选 RustFS/S3），全文检索用 Tantivy。详见 `spec/technical_solution.md` 与 `spec/product_design.md`。

## 命令

所有命令都在仓库根目录执行。日常开发、构建、测试都走顶层 `Makefile`（不要直接调 `cargo leptos`，那要绕开一堆 cargo-leptos 自动注入的 env）。

```bash
make dev           # 开发服务器，cargo-leptos watch，改文件自动热重载
make serve         # 非 watch 模式启动（用于手工跑构建产物）
make build         # 本地构建（debug）—— 产物在 target/site 与 target/debug/rodeo
make build-release # 本地 release
make test          # cargo test（ssr target）
make check         # 类型检查：先 native 再 cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown
make clippy        # cargo clippy --all-targets --all-features
make fmt           # cargo fmt
make clean         # cargo clean（包含 target/site）
make reset-data    # rm -rf data（bincode 结构变更、调试需要时使用）
```

构建发行物：

```bash
make build-linux        # Docker 内交叉编译，产物 dist/rodeo + dist/site/
make build-linux-host   # 宿主 zig 交叉编译，更快但只出二进制（不出 site）
make package-linux      # 打成 dist/rodeo-<arch>.tar.gz，含 start.sh
```

`make help` 列出全部目标。所有 Linux/网络细节（zig musl 头、registry mirror、下载代理）都在 `Makefile` 注释里——如果要新加交叉编译选项，先读那一段。

修改/Cargo.toml 之后记得同步 `Makefile` 里的 `LEPTOS_BIN_ENVS` 与 `package.metadata.leptos`（服务端二进制用 env，对应字段名一一映射）。

## 验证回路

按用户偏好：**不要补新的 Rust 单元测试**。改完代码以这两条命令作为通过门槛：

```bash
make check        # native + wasm 两侧类型检查
make build        # 完整编译（debug 即可，发现不了 wasm-only 类型问题，但能跑通 SSR 路径）
```

前端烟雾测试与浏览器校验的脚本约定见 `~/.claude/.../memory/browser-verification-setup.md`（playwright-core + `LEPTOS_SITE_ROOT=target/site-smoke`）。

## 代码地图

### 顶层 crate 与特性

`src/lib.rs` 是 cdylib + rlib 共享 crate；`src/main.rs` 只在 `feature="ssr"` 下编译，`src/lib.rs` 的 `hydrate()` 只在 `feature="hydrate"` 下编译。两个特性互斥：开发时 `cargo leptos` 把 server 二进制按 `bin-features=["ssr"]` 编，把 wasm 库按 `lib-features=["hydrate"]` 编。

`Cargo.toml` 里有一个**双 rustls provider 必须崩**的隐性约束——`axum-server`、`reqwest`、`object_store` 全走 `rustls` 而不能引入 `aws-lc-rs`，否则 `ServerConfig::builder()` 在多 provider 下 panic。动这三块依赖时先看 `Cargo.toml` 注释，再确认 `rustls = { default-features=false, features=["ring"] }` 没被改。

### 三层目录

`src/` 严格按 spec 第 2.2 节走三层分离：

```
src/
├── app.rs        # Leptos 根组件，路由表（/, /login, /workspaces, /admin, /account, /:slug, /:slug/entry/:code, /:slug/settings）
├── golayout.rs   # 布局工具
├── api/          # HTTP 层
│   ├── graphql.rs     # async-graphql schema + 解析器（最大文件 ~86k 行）
│   └── attachments.rs # 唯一的附件下载路由（免鉴权能力 URL，content-type 白名单见文件顶部）
├── service/      # 业务层（在 mod.rs 聚合到 Services 结构体）
│   ├── mod.rs    #   启动顺序决定一切：workspace → message → entry → comment → attachment → 之后才建 label/audit/view/ai/rule
│   ├── auth.rs, entry.rs, label.rs, view.rs, query.rs, rule.rs, workspace.rs
│   ├── attachment.rs, comment.rs, message.rs, mention.rs, search.rs, ai.rs, audit.rs
├── storage/      # 持久化层
│   ├── doc.rs       # DocStore 门面（列族 cf::*，批量 BatchOp）
│   ├── blob.rs      # BlobStore（local / rustfs-s3）
│   ├── rocksdb.rs   # DocBackend::Rocksdb 实现
│   ├── pg.rs        # DocBackend::Postgres 实现（同步 API，每个连接独占线程）
│   ├── keys.rs      # 共享的键前缀 / ULID 转换
│   └── mod.rs       # 只导出 DocStore / BlobStore，pg/rocksdb 细节不出模块
├── domain/       # 纯类型（Entry, Label, View, Query AST, Rule, AuditLog, …），serde 派生给前后端共用
├── frontend/     # 仅 hydrate 目标使用
│   ├── graphql_client.rs   # WASM 侧请求 /api/graphql（gloo-net），token 存 localStorage
│   ├── components.rs       # AppBar 与共享小件
│   ├── pages/              # account, admin, login, workspaces, workspace_main, settings, entry
│   ├── query_eval.rs       # 服务端 query 表达式的客户端复现（前端过滤也要走同一套 AST）
│   ├── label_editor.rs, view_filter.rs, timeline.rs, automation_tab.rs, comment_list.rs, attachment_list.rs
│   ├── ai_prompt_editor.rs, tiny_editor.rs (包装 @opentiny/tiny-editor web component)
│   └── icons.rs            # 内联 SVG
├── config.rs     # TOML 反序列化（所有字段都有 #[serde(default)]）
├── error.rs      # AppError 统一错误类型，传染到 GraphQL
└── main.rs / lib.rs
```

### 关键交叉点

- **`service/mod.rs` 的启动顺序是有依赖的**：message 依赖 workspace 的成员表；comment、attachment 必须拿到 entry 的 Arc 才能在变更时回写 `updated_at` 与重索引。`Services::new` 末尾还有一连串 idempotent 的 backfill/repair（label 模式修补、二级索引灌库、view 排序字段的 bincode 转码），见同文件注释。
- **存储的「kv + 列族」抽象**在 `storage/doc.rs`。Postgres 后端在 `pg.rs` 把同一组列族落成 kv 表。改 schema 时两边都要动。
- **Query 表达式 AST**（`domain/query.rs`）前后端共用：`RESERVED_FIELDS` 与 `Condition/Field/Op` 一份类型，server 端解析后执行，前端 `frontend/query_eval.rs` 复算用于过滤本地缓存。
- **标签**（`domain/label.rs`）：`LabelSchema` 定义 + `LabelValue` 受 schema 约束；UI 在 `frontend/label_editor.rs`。enum/boolean/integer/float/string/null 五种 `LabelValueType`。
- **规则引擎**（`domain/rule.rs` + `service/rule.rs`）：打标签触发 `RuleEngine` 跑规则、产生二次 LabelWrite；UI 在 `frontend/automation_tab.rs`。
- **`/api/attachments/{id}` 永远不带 auth**（见 `api/attachments.rs` 顶部注释），因为浏览器 `<img src>` 带不了 Authorization 头。安全靠 ULID 不可猜 + content-type 白名单（只放行图片）+ 不回声上传方上报的 mime。

### 实时与多端同步

不再用 WebSocket（spec 早期提过），最近 commit 一直走 GraphQL polling：`d94a4a1 fix(frontend): mark-read refresh + 清理轮询定时器` 描述了当前做法。`mark-read`、`recent` 等异步路径都是轮询而非订阅。

## 配置

- 默认配置 `config.example.toml`（进仓库）；本地 `config.toml` 走 `.gitignore`。
- `LEPTOS_SITE_ADDR` 环境变量**整体覆盖** `[server]` 的 host/port，并在启动日志里打印来源。`cargo leptos watch` 自动注入它（dev 永远 127.0.0.1:3000）；手工运行 `./rodeo config.toml` 时才会真的用 `config.toml` 的 `[server]`。
- `[server.tls]` 整段不存在→明文 HTTP；存在→ALPN 协商 http/2。证书不进仓库（`/certs` 忽略）。
- `[storage.doc]` 后端 `rocksdb | postgres`；`[storage.blob]` 后端 `local | rustfs`。两者都改时，附件的对象键 `{workspace_id}/{entry_code}/{id}_{文件名}` 不变，所以换个桶就是把目录搬过去，没有迁移脚本。
- `[ai]`：`api_key` 或 `model` 留空就视作 AI 未启用——生成总结时会返回「服务端未配置 AI 模型」。所有 HTTPS 调用一律走 rustls，私有 CA 没法关校验。

## 注意事项

- **服务端用 `current_thread` 运行时**，不要在异步上下文里写阻塞调用（DocStore 已经都是同步）。`storage/pg.rs` 注释里描述了同步 Postgres 客户端如何独占一个 OS 线程。
- **bincode 位置编码**：`LabelValueType` 这种枚举只能追加新变体在末尾，否则旧记录反序列化失败。`service/mod.rs` 启动时的 `repair_*` 方法就是为应对历史上这类破坏；新增带数据的变体请同时加 idempotent 修复。
- **标签自动激活**：登录态存于 `localStorage.rodeo_jwt`（仅 WASM）。`app.rs` 里 `me()` 拿到 `Ok(None)` 时必须把死 token 清掉 + 置 `session_lost`，不然 `/admin`、`/account` 永远停在加载态。
- **`Style` 走 `style/main.css`**，通过 `HashedStylesheet` 拼接 hash；不要手动 `<link rel=stylesheet href=...>` 主样式。
- **`tiny-editor` 静态资源**经 `/tiny-editor/style.css` 与 `/tiny-editor/glue.js` 提供，从 `public/` 出。这些被引用在 `app.rs` 的 `<Stylesheet>` / `<Script>` 里。
- **Leptos 上下文只向下传播**：AppBar 渲染在 `<Router>` 之外，与 WorkspaceMain 是兄弟节点。`provide_workspace_new_menu_slot()` / `provide_workspace_timeline_slot()` 这两个 slot 是为了解决「App 级别提供、兄弟共享读写」用的，新加跨 Router 的全局能力先沿用同一模式。
- **`/api/graphql` body limit 64MiB**，给 multipart 边界留余量；附件本身在服务层 50MiB 拒绝。

## 预先索引好的工具

- `.codegraph/` 有预构建的 SQLite 知识图。在改代码前优先用 `codegraph_explore`，比 `Read` + `Grep` 循环快得多。
- `.superpowers/` 与 `.claude/projects/.../memory/` 已存有项目级约束（不补 Rust 单测、build host 是 Intel MacBook Air、Docker 缓存 GC、修复旧数据优先于读兼容、`书签`=标签、查询表达式用 AND/OR/NOT 而不是「且/或/非」、数据密集页去掉 1280px 顶部 cap 等）。改相关代码前看一下对应 memory 全文。
- `spec/` 与 `docs/` 里那些「补充设计」「缺陷记录」的日期文件记录了当时的非显然决定；任务是「修复」就直接动手，不需要再问格式。
