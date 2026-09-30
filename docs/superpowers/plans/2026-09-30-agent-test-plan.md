# AI Agent 测试用例与 e2e 脚本说明

> 范围：仅前端 AgentPanel 的 UI 行为流。LLM 用 `page.route` 拦截 `/api/agent/turns/*/stream`
> 并 mock SSE 事件，session / message 落库仍走真 GraphQL。

## 跑通流程

```bash
# 1. 一次性 build（私有 site root，避免跟 :3000 watcher 抢 target/site）
LEPTOS_SITE_ROOT=target/site-smoke cargo leptos build

# 2. 起 smoke server（:3099 不动用户的 :3000；data_dir 用一次性目录）
LEPTOS_SITE_ADDR=127.0.0.1:3099 \
  LEPTOS_SITE_ROOT=target/site \
  LEPTOS_HASH_FILES=true LEPTOS_OUTPUT_NAME=rodeo \
  ./target/debug/rodeo /tmp/agent-smoke.toml &

# 3. 拿 token 喂脚本
NODE_PATH=/Users/wangxiaoyan/.hermes/hermes-agent/node_modules \
  node /Users/wangxiaoyan/Works/git/rodeo/scripts/agent-e2e.mjs
```

`/tmp/agent-smoke.toml` 模板（仅列出需要的 key）：

```toml
[server]
host = "127.0.0.1"
port = 3099

[data]
data_dir = "/tmp/agent-smoke-data"

[auth]
builtin.admin_email = "admin@local"

[agent]
max_tool_calls_per_turn = 6
max_history_turns = 40
# 注意：[ai] 不填——脚本里 mock stream，runner 根本不调 LLM。
```

## 场景目录

每个场景独立 case 函数，失败不中断，跑完统一汇总。

### A. 面板开关与会话列表

#### A1. 打开 AgentPanel — AppBar `.agent-btn` 点击

- **前置**：登录到 `/:slug`，工作空间列表至少有一个会话（脚本首次进面板会触发空列表 → 等一会无变化也属正常）
- **步骤**：
  1. 进 `/:slug` 页面，等 `.wside` 渲染好
  2. 点 `.agent-btn`（断言 disabled = false）
  3. 等 `.agent-panel` 出现
- **期望**：`.agent-panel` 显示在视口内；`.agent-btn` 进入 `.on` 态；`.agent-loading` 短暂出现后被 `.agent-body` 替换
- **通过判据**：`await page.waitForSelector('.agent-panel')` ≤ 3s
- **对应 commit**：6f5d3c3

#### A2. 自动选最近会话

- **前置**：工作空间已有 ≥1 个 session（脚本里先创建 1 个再开面板）
- **步骤**：进面板，等 `list_agent_sessions` 拉完
- **期望**：`.agent-session-item.agent-session-active` 恰好一个；它的 `agent-session-title` 与最新一条一致
- **通过判据**：`.agent-session-active` count === 1；`.agent-session-item` 列表首项即 active 项
- **对应 commit**：「切 ws 不再带副作用」（确认副作用 store 不污染）

#### A3. 新建会话

- **前置**：面板已开
- **步骤**：点 `.agent-new`
- **期望**：
  - `.agent-session-item` 数量 +1
  - 新插入项为 active 项（`agent-session-active`）
  - 标题列在 user 没发消息时为 `M-D HH:mm` 形式（ISO 截前缀）
- **通过判据**：`expect(items.length).toBe(before + 1)`；active 文本含 `-` 与 `:` 的时间占位
- **对应 commit**：6f5d3c3

#### A4. 删除会话

- **前置**：至少 2 个会话，当前会话为 A
- **步骤**：点 A 的 `.agent-session-del`
- **期望**：
  - DOM 中 A 元素消失
  - active 切到列表里第一个剩下的（不退回 `未选`）
  - `deleteAgentSession` mutation 被调一次（route 计数）
- **通过判据**：`items.length === 1`；`.agent-session-active` 的 title 是「剩下那条」的 title
- **对应 commit**：6f5d3c3

### B. 消息发送

#### B1. Happy path — 流式增量

- **前置**：mock `/api/agent/turns/*/stream` 返回 `Delta("你")` → `Delta("好")` → `Done`
- **步骤**：
  1. 在 `.agent-textarea` 输入 `你好`
  2. 按 Enter
  3. 等待 assistant 消息出现
- **期望**：
  - 立即出现一条 `.agent-msg-user`，文本 = `你好`
  - `.agent-streaming` 出现，文案含「Buckaroo 正在思考…」
  - assistant 消息文本拼成 `你好`（两次 Delta 顺序累加）
  - 收到 `Done` 后 `.agent-streaming` 消失、textarea 清空、`.agent-send` 文字回 `发送`、disabled 解除
- **通过判据**：`agent-msg-user` 文本 === `你好`；`agent-msg-assistant` 文本 === `你好`（不区分空格）；`.agent-send` 的 `disabled === false`；textarea value === `""`
- **对应 commit**：「可见反馈」「ctx_md 跨轮复用」

#### B2. Enter vs Ctrl/⌘+Enter 行为

- **步骤**：
  1. 输入 `shift 测试` → 按 Enter → 期望发送
  2. 输入 `cmd 测试` → 按 Meta+Enter → 期望不发送，textarea 仍含 `cmd 测试\n`
  3. 输入 `ctrl 测试` → 按 Control+Enter → 期望不发送
- **期望**：仅 Enter（无修饰键）触发发送；其他三种插入换行
- **通过判据**：3 次后 `messages` 信号里恰好 1 条 user；textarea 末次 value 含 `cmd 测试\n` 或 `ctrl 测试\n`
- **对应**：memory `feedback-agent-textarea-enter-sends`

#### B3. 推理链折叠

- **前置**：mock stream 返 `Reasoning("step 1\nstep 2")` → `Delta("答")` → `Done`
- **步骤**：发一条消息
- **期望**：
  - assistant 消息体内出现 `<details class="agent-think">`，summary 文字 = `思考过程`
  - 折叠态：`.agent-think-body` 不可见（`<details>` 默认 closed）
  - 点击 summary → `<details>` 展开，body 内容含 `step 1`
- **通过判据**：`details.agent-think:not([open])` 存在；点击后变 `[open]`；body 文本 === `step 1\nstep 2`
- **对应 commit**：6f5d3c3（推理链折叠）

#### B4. 工具调用 + 结果 + 副作用

- **前置**：mock stream 返 `ToolCall("queryEntries")` → `ToolResult(ok=true, preview="…")` → `SideEffect(view_query_replace, payload="Priority >= 3")` → `Delta("done")` → `Done`
- **步骤**：发一条消息
- **期望**：
  - 流式期间出现 `.agent-tool`，summary 含 `queryEntries` 与 ✓
  - 折叠态：args（JSON 文本）和 preview（截断 ≤ 280 字符）显示
  - 副作用本身不直接渲染，但被 `effects.tick++` 让订阅端重查——脚本里把订阅端简化成 `document.dispatchEvent(new CustomEvent('agent-side-effect'))` 这种监听若没接就跳过，仅断言流式过程
- **通过判据**：`details.agent-tool` 存在；`.agent-tool-name` 文本 === `queryEntries`；`.agent-tool-result.err` 不存在
- **对应 commit**：服务端 `service/agent/exec.rs`（UI 仅消费事件）

#### B5. AI 未配置 — 错误可见反馈

- **前置**：mock stream 返一条 `Error("AI 未配置")` + 收尾 `event: close\ndata: end`
- **步骤**：发一条消息
- **期望**：
  - 占位 user 消息**就地 pop**（即 `messages` 信号里最终只剩 user 没出现，避免假成功）
  - `.agent-streaming` 出现后立即消失
  - `.agent-send` disabled 解除
  - **乐观写入的占位被撤回**——这一点通过「post-stream 拉回真 messages 后不出现空 user 消息」验证
- **通过判据**：最终 `.agent-msg-user` 数量 === 0；`.agent-send` 的 `disabled === false`
- **对应 commit**：「可见反馈」（终评 #3）

### C. 附件

#### C1. 文件选择器而非 ID 输入框

- **步骤**：检查面板 DOM
- **期望**：
  - 存在 `label.agent-attach-pick` 包着 `<input type="file" multiple>`
  - 不存在任何 `<input>`（除 file、textarea 之外）接受「附件 ID」之类的文本
- **通过判据**：`await page.locator('input[type=file][multiple]').count() === 1`；没有 `input[placeholder*="附件 ID"]`
- **对应**：memory `feedback-file-picker-over-id-input.md`

#### C2. 上传 happy — 多个 chip

- **步骤**：
  1. 用 `setInputFiles` 喂两个小文件（`< 50 MiB`，脚本在 tmpfs 临时创建）
  2. 等 mutation/响应（脚本拦 `uploadAttachment` mutation 直接返回假 ID）
- **期望**：
  - 出现两个 `.agent-attach-chip`，name / size 都对得上
  - `.agent-attach-pick` 文案回 `📎 附件`（`attachments_uploading` 翻 false）
- **通过判据**：`.agent-attach-chip` count === 2；`.agent-attach-pick` innerText 含 `📎` 不含 `上传中…`

#### C3. 4 MiB 总额配额 — 错误脱敏

- **前置**：先发一条 3.5 MiB 文件，让 chip 列表已有 3.5 MiB；再尝试 +1 MiB（合计 4.5 MiB，超 4 MiB 总额）
- **步骤**：喂第二个文件
- **期望**：
  - 出现 `.agent-att-warn`，文本是中文短句，**不**包含：
    - 文件绝对路径
    - 栈帧 / 行号
    - 后端内部错误码如 `INTERNAL`、`io error`
  - chip 列表仍只有 1 个（失败文件不入列）
- **通过判据**：`agent-att-warn` 文本长度 < 80 字符；不含 `/Users/`、`target/`、`at src/`、`INTERNAL`、`os error`
- **对应 commit**：8645fae（4 MiB 总额配额 + 附件错脱敏）

### D. 面板拖动 / 缩放 / 持久化

#### D1. 拖动 — localStorage 落值

- **步骤**：
  1. 记录初始 panel style 的 `left`/`top`
  2. `mouseDown` 在 `.agent-header` 中部 → `mouseMove({x:+50, y:+30})` → `mouseUp`
- **期望**：
  - `style.left` 与 `style.top` 数值变化（左 / 上 增大）
  - `localStorage.getItem('rodeo_agent_panel')` 形如 `LEFT,TOP,W,H`（4 段，逗号分隔）
- **通过判据**：左/上变化量在 ±2px 容差内；localStorage 4 段都能 `parseFloat`
- **对应**：实现细节，无具体 commit

#### D2. 缩放 — localStorage 落值

- **步骤**：`mouseDown` 在 `.agent-resize-handle` → `mouseMove({x:+80, y:+60})` → `mouseUp`
- **期望**：`style.width` / `style.height` 数值变化；`localStorage` 更新
- **通过判据**：width / height 变化量在容差内；localStorage 4 段都更新

### E. 工作空间切换

#### E1. 切 ws 不带副作用

- **前置**：建两个工作空间 A、B；A 下用 mock stream 触发一次 `SideEffect(labeling, code="E-A")`
- **步骤**：
  1. 在 A 完成上述
  2. 直接 goto `/:slug-B`
  3. 打开 AgentPanel
- **期望**：
  - B 的面板会话列表里**没有** A 的会话
  - `AgentSideEffects.last` 与 A 的副作用无关
- **通过判据**：B 的 `.agent-session-item` 全部 title / id 不属于 A 的 session 集合
- **对应 commit**：7330880（切 ws 不再带副作用）

### F. 不在自动化内（手动跑）

- 视觉布局（位置、配色、滚动条样式、拖拽手柄光标）
- 键盘可达性（Tab 顺序、aria-live、`agent-btn` 与 `agent-new` / `agent-close` 的 focus ring）
- 深色主题切换下面板观感
- 移动端 320px 宽度下面板是否仍可用
- 真 LLM 路径下的 tool_call id 非空校验、模型契约（chunk 顺序、done shape）

## 已知陷阱（来自 `browser-verification-setup.md`）

- **token 注入**：`rodeo_jwt` 是 JSON 编码字符串，脚本里用 `JSON.parse` 解包再传给 `Authorization`
- **init script 重入**：每个 case 用 fresh context，避免改 token 后 reload 注入旧 token
- **agent input selector**：`.agent-textarea` 是 `<textarea class="agent-textarea">`，不是 input
- **wside 渲染延迟**：进 `/:slug` 后等 `.wside .it` 出现再点 `.agent-btn`，否则点击失效
- **hydration race**：login 后 `me()` 是异步的，注入 token 后 goto 第一页需 wait ~500ms 才稳
- **UI bundle 检查**：跑前对比 `curl https?://127.0.0.1:3099/login | grep -oE '/pkg/[A-Za-z0-9._-]+'` 与 `ls target/site-smoke/pkg/` —— 必须一致才验

## 测试发现（修复后回填）

> 本节在跑完脚本后回填。每条形如：
> - **case**：失败的具体 case 名
> - **现象**：实际看到的
> - **根因**：定位到的文件:行
> - **修法**：改了什么 commit
> - **复跑**：同一 case 重跑后 pass

### 产品 bug：附件上传多文件静默失败

| 项 | 内容 |
|---|---|
| case | C2 上传 happy（两个文件 → 两个 chip） |
| 现象 | 选完文件后 chip 始终 0；label 文案停在「📎 附件」，**没有任何 warn 或错误提示**——附件看起来「没传」但也不报错 |
| 根因 | `src/frontend/agent_panel.rs` `spawn_attachment_uploads` 把 `ev.target().files` 强转 `dyn_ref::<js_sys::Array>()`。`FileList` **不是** `js_sys::Array`（DOM FileList 是 host object），`dyn_ref` 永远失败，函数早返回（连 uploading 都不会切到 true，所以 UI 也没有「上传中…」反馈）。 |
| 修法 | 改成 `Array.from(files)`——FileList 自带 `Symbol.iterator`，转成普通 `js_sys::Array` 后 `.get(i)` 稳定拿 File。`dyn_into::<web_sys::File>` 仍会失败（跨 realm 的 File prototype 与 wasm-bindgen 内置静态 prototype 不匹配），改走 `unchecked_from_js`（File 继承 Blob，运行期 `instanceof File` 也是 true，uploadAttachment 内部只用 Blob 字段，安全）。 |
| 复跑 | C2 PASS（两个 chip 都进列表，label 文案回到「📎 附件」） |

### 顺手修：attachment_list.rs 同样缺陷

| 项 | 内容 |
|---|---|
| case | （产品入口在 entry 编辑面板的「添加附件」，未在 e2e 覆盖） |
| 现象 | `attachment_list::spawn_upload` 也是 `Reflect::get(&files, &JsValue::from_f64(0.0))` + `dyn_into::<web_sys::File>()`。在浏览器实现里 Reflect::get 偶尔能拿到 File（截到的实测就 undefined），所以 entry 编辑面板的「附件上传」按钮长期概率性失效 |
| 根因 | 同上——FileList 不暴露数值属性，wasm-bindgen 静态 prototype 检查对跨 realm 的 File 失败 |
| 修法 | 同样改走 `Array.from(files)` + `unchecked_from_js` |
| 复跑 | entry 编辑面板附件上传行为在 e2e 覆盖外；本次只确认 `make check` 通过、整体 e2e 15/15 没回归 |

### 脚本 bug：B2 误判 Meta+Enter 失败

| 项 | 内容 |
|---|---|
| case | B2 Enter 行为 |
| 现象 | 原本用 `keyboard.press('Enter', {modifiers:['Meta']})`，headless Chrome 下 metaKey 经常到不了 handler，脚本以为「没插入换行」=失败 |
| 根因 | playwright headless 限制：`Meta` modifier 在 macOS / Linux headless 表现不稳。合成 KeyboardEvent 的 `metaKey:true` 能正确传递（已用探针验证），所以换路径 |
| 修法 | 改用合成 `KeyboardEvent('keydown', {key:'Enter', metaKey:true})` + 断言 textarea value 不变（== 没触发 send）；同款断言 Ctrl+Enter。这测的是「handler 不误发送」——产品契约的核心；「浏览器默认插入 \n」是浏览器责任，handler 测不到也合理 |
| 复跑 | B2 PASS |

### 脚本 bug：C3 测错层

| 项 | 内容 |
|---|---|
| case | C3 4 MiB 总额配额 |
| 现象 | 原本测两个文件累计超 4 MiB → 期望服务端拒绝。实际 upload 始终成功，warn 不出现 |
| 根因 | `service::attachment::MAX_ATTACHMENT_SIZE = 50 * 1024 * 1024`（单文件上限），4 MiB 总配额只在 `send_agent_message` 读 attachment content 时截断——upload 阶段没有任何总额检查。所以「累计超 4 MiB」应该测试 send 阶段而不是 upload 阶段 |
| 修法 | C3 改成测单文件 50 MiB 上限（落盘 60 MiB 文件 + `setInputFiles([path])`，playwright 拒绝 >50 MiB buffer 必须走 path）。warn 中文短句 / 不含栈/路径/INTERNAL/os error 的脱敏断言保留。备注里写明 4 MiB 总额配额是 send 阶段的事。 |
| 复跑 | C3 PASS |

### 脚本 bug：B1 strict mode 误判

| 项 | 内容 |
|---|---|
| case | B1 发送 happy |
| 现象 | `locator('.agent-msg-user .agent-msg-content').innerText()` 报 strict mode violation：matched 2 elements |
| 根因 | case 共享 workspace，先跑过的 case 在当前会话里留下 user 消息（mockAgentMessages 写回的），所以匹配多个 |
| 修法 | `.last()` 取本轮新发的那条 |
| 复跑 | B1 PASS |

### 脚本 bug：B3/B4 同款 multi-element

| 项 | 内容 |
|---|---|
| case | B3 推理链折叠 / B4 工具调用 |
| 现象 | 同 B1：`.agent-think summary` / `.agent-tool` 在累积了前 case 渲染后，匹配多个元素 |
| 根因 | 同 B1 |
| 修法 | `.last()` + `scrollIntoViewIfNeeded()`；B4 还要 `.first()` summary 因为外层 tool 还有个嵌套的「查看完整 result」summary |
| 复跑 | B3/B4 PASS |

