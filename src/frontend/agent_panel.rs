//! AI Agent 侧拉面板：会话列表 + 消息流 + 输入框。
//!
//! 用例见 spec §14。最小可用 UI 已完整：会话列表 + 切换 + 删除、Markdown
//! 消息渲染、tool_call / tool_result 气泡、按角色 + kind 上色。剩余的
//! UX 收尾（自动滚动 / 长 tool_call 折叠 / loading spinner）不在
//! 当前 scope。
//!
//! 数据来源：
//! - GraphQL：`list_agent_sessions` / `list_agent_messages` /
//!   `create_agent_session` / `delete_agent_session` / `send_agent_message`
//!   （`crate::frontend::graphql_client`，Task 10 接入）
//! - SSE：`/api/agent/turns/<turn_id>/stream`（Task 7/8），用 `fetch +
//!   ReadableStream` 自己解析（详见 `open_stream` 注释）—— 浏览器
//!   `EventSource` 不能设 `Authorization` 头，必须走 `fetch` 自己补。
//!   **流式读取**：`ReadableStreamDefaultReader` + `TextDecoder('utf-8', {fatal:false})`
//!   增量拉 chunk，buffer 拼到 SSE 双换行就处理一条事件；这样 delta 一到
//!   浏览器就立即 render，用户体感是「AI 在一个字一个字写」，而不是「等 30s
//!   之后刷一下全出来」。
//!
//! 副作用派发：`crate::frontend::agent_side_effects::AgentSideEffects`
//! —— 服务端推 `side_effect` 事件时更新 `tick` + `last`，订阅端据此重查。
//!
//! Drift 修正（与 Task 11 brief 对照）：
//! 1. brief 用了不存在的 `WorkspaceContext`；本仓库 `Workspace` 只能从
//!    URL slug 通过 `workspace_by_slug` 异步取，于是 AgentPanel 把
//!    workspace_id 包成 `Resource` 后再驱动后续请求；workspace_id 未就绪
//!    时整面板显示「加载中…」。
//! 2. brief 在 setup 段 `current_session.get()` + `else { return; }`，在
//!    `on_send` 闭包里改成 `get_untracked()` 拿到当时的值——闭包不会随
//!    signal 变化自动重跑，靠 `move` 一次快照即可。

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_location;

#[cfg(target_arch = "wasm32")]
use serde_json::Value;

use crate::frontend::agent_side_effects::AgentSideEffects;
#[cfg(target_arch = "wasm32")]
use crate::frontend::agent_side_effects::SideEffectHint;
#[cfg(target_arch = "wasm32")]
use crate::frontend::graphql_client::get_token;
use crate::frontend::components::human_size;
use crate::frontend::graphql_client::{
    create_agent_session, delete_agent_session, list_agent_messages, list_agent_sessions,
    send_agent_message, AgentMessage, AgentMessageContext, AgentSession,
};
#[cfg(target_arch = "wasm32")]
use crate::frontend::graphql_client::upload_attachment;

/// 已选附件（上传成功后存 id）。发消息时按 id 列表传给 sendAgentMessage。
#[derive(Clone)]
struct AgentAttachmentChip {
    id: String,
    filename: String,
    size: i64,
}

/// 流式 ToolCall 卡片渲染态。ToolCall 事件到达 → push 新元素（status=Running）；
/// ToolResult 事件到达 → 按 name（FIFO 配对）找到对应元素更新 status + preview。
/// 渲染时取前 4 个并排 grid，5+ 折叠进 <details>——见 Task 24 brief。
///
/// `args` 当前仅在流式期间写入 ToolCallDisplay,卡片视图只渲染 name + status;
/// 留字段是为了和后端 AgentEvent::ToolCall 对齐,后续若加「卡片可点击展开
/// args 预览」就直接读,不需要再补回字段。
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct ToolCallDisplay {
    id: String,
    name: String,
    args: serde_json::Value,
    status: ToolCallStatus,
    preview: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolCallStatus {
    /// 工具调用刚发出，还未拿到结果。
    Running,
    /// 工具调用成功完成（含 ToolErrorKind::Ok）。
    Ok,
    /// 工具调用失败（bad_args / rejected / server_error 任一）。
    Err,
}

/// 是否展开 Agent 面板。`true` 时显示、点击关闭按钮或外部 trigger 置 `false`。
///
/// 上下文「开关」信号——与 AuthState / WorkspaceNewMenuSlot 同款，由调用方
/// 在 App 层 `provide_context`，组件内 `use_context` 取出来用。
#[derive(Clone, Copy)]
pub struct AgentPanelOpen(pub RwSignal<bool>);

pub fn provide_agent_panel_open() -> AgentPanelOpen {
    let s = AgentPanelOpen(RwSignal::new(false));
    provide_context(s);
    s
}

pub fn use_agent_panel_open() -> AgentPanelOpen {
    use_context::<AgentPanelOpen>().expect("AgentPanelOpen 未提供")
}

/// 工作区侧把当前视图 id + 表格勾选条目 code 列表传进 AgentPanel；
/// 由 `WorkspaceMain` 在 setup 段 `provide_context`，AgentPanel 拿不到就 None
/// ——非工作区路由（登录 / 账号页）不发上下文给后端，避免污染 agent 总结。
#[derive(Clone, Copy)]
pub struct AgentContext {
    pub view_id: RwSignal<Option<String>>,
    pub selected_entry_codes: RwSignal<Vec<String>>,
}

pub fn provide_agent_context(
    view_id: RwSignal<Option<String>>,
    selected_entry_codes: RwSignal<Vec<String>>,
) -> AgentContext {
    let ctx = AgentContext {
        view_id,
        selected_entry_codes,
    };
    provide_context(ctx);
    ctx
}

/// 非 panic 兜底——AgentPanel 挂在 App 层，工作区没挂载时拿不到，
/// 视为「无上下文」即可。
pub fn use_agent_context() -> Option<AgentContext> {
    use_context::<AgentContext>()
}

#[component]
pub fn AgentPanel() -> impl IntoView {
    let open = use_agent_panel_open();
    let effects = use_context::<AgentSideEffects>().expect("AgentSideEffects 未提供");
    let agent_ctx = use_agent_context();

    let location = use_location();
    // workspace_by_slug 拿到的 workspace_id：所有会话/消息请求都依赖它。
    // `use_location()` 而非 `use_params_map()`：AgentPanel 挂在 Router 内、
    // <Routes> 之外（与 <main> 同级），不处于任何匹配 Route 的子树里，
    // `use_params_map()` 会 panic 在"outside the context of a matched <Route>"；
    // `use_location()` 在 Router 内任何位置都能给出当前 pathname，从路径首段
    // 解 slug："/foo" → "foo"；"/login" → ""（workspace_id 为 None）。
    // 把 `workspace_id` 直接放进 RwSignal —— Resource 在 native target 下要求
    // future 实现 `Send`，而 `gloo_net::http::Request` 内部是 `Rc<RefCell>`，
    // 即使 cfg-gated 也会让 trait solver 把整条 future 推到 `!Send`。改用
    // Effect + spawn_local 直接拉一次，与 SSR 路径上的 workspace_by_slug
    // 行为一致：native 端 graphql() 返回 Err → 这个 Effect 在 SSR 期间无
    // 网络也没副作用，到 hydrate 后浏览器真实运行时会拿到 workspace_id。
    let workspace_id_resolved: RwSignal<Option<String>> = RwSignal::new(None);
    {
        let location = location.clone();
        Effect::new(move |_| {
            let slug = first_path_segment(&location.pathname.get());
            if slug.is_empty() {
                workspace_id_resolved.set(None);
                return;
            }
            spawn_local(async move {
                let res = crate::frontend::graphql_client::workspace_by_slug(&slug).await;
                let (next, log) = match res {
                    Ok(Some(ws)) => (Some(ws.id), None),
                    Ok(None) => (
                        None,
                        Some(format!("workspace_by_slug slug={slug:?} → 不存在")),
                    ),
                    Err(e) => (
                        None,
                        Some(format!(
                            "workspace_by_slug slug={slug:?} → {}: {e}",
                            classify_workspace_error(&e)
                        )),
                    ),
                };
                if let Some(m) = log {
                    web_sys_console_warn(&m);
                }
                workspace_id_resolved.set(next);
            });
        });
    }

    // 会话列表 / 当前会话 / 消息 / 输入 / 流式状态 —— 全部 RwSignal，
    // 闭包里靠 .get_untracked() 一次性快照（见 on_send 注释）。
    let sessions: RwSignal<Vec<AgentSession>> = RwSignal::new(Vec::new());
    let current_session: RwSignal<Option<String>> = RwSignal::new(None);
    let messages: RwSignal<Vec<AgentMessage>> = RwSignal::new(Vec::new());
    let input: RwSignal<String> = RwSignal::new(String::new());
    // 已上传附件（按用户选文件顺序累加）；发消息时按 id 列表传给 sendAgentMessage。
    let attachments: RwSignal<Vec<AgentAttachmentChip>> = RwSignal::new(Vec::new());
    // 一次选中多文件时多协程并发上传，UI 用它显示「上传中…」+ 禁用发送按钮。
    let attachments_uploading: RwSignal<bool> = RwSignal::new(false);
    // 上传失败的提示（resolver 拒绝 / 网络错 / 超 50 MiB）。单条短提示，替换不堆叠。
    let attachments_error: RwSignal<Option<String>> = RwSignal::new(None);
    let streaming: RwSignal<bool> = RwSignal::new(false);
    // 流式 ToolCall 卡片列表：tool_call 事件 push 进来、tool_result 事件
    // 按 name（FIFO）配对更新。list_agent_messages 重新拉到真消息后清空——
    // 真消息的 m.tool_calls 字段自带完整配对，不需要乐观态再渲染。
    // 本信号非 cfg(target_arch = "wasm32") 也持有：SSR / native 编译下 handle_event
    // 不跑，列表始终为空，零开销。
    let streaming_tool_calls: RwSignal<Vec<ToolCallDisplay>> = RwSignal::new(Vec::new());
    // 用户是否「贴底」：scroll 事件每次把它更新成 (scroll_height - top - client) < 32px。
    // 初始 true —— 切会话 / 首次打开面板时直接滚到底，而不是停在历史某处。
    // 自动滚动 effect 只在 at_bottom 时推进 scroll_top，避免用户主动上滑读历史时跳回去。
    // 仅 wasm32 持有：native / SSR 编译时既无 DOM 也无滚动事件，整组都关掉。
    #[cfg(target_arch = "wasm32")]
    let messages_at_bottom: RwSignal<bool> = RwSignal::new(true);
    let messages_ref: NodeRef<leptos::html::Div> = NodeRef::new();

    // 浮动面板的位置 / 尺寸 —— 拖拽与缩放的工作对象。
    // 默认值是「右下角浮窗」占位坐标，第一次打开时由下面的 Effect 用
    // 视口尺寸 + localStorage 落值覆盖掉。常量取「小一点」的高度（480px）
    // 满足 spec「默认高度可以更小些」，宽 520 与旧版一致避免布局跳。
    let panel_left: RwSignal<f64> = RwSignal::new(40.0);
    let panel_top: RwSignal<f64> = RwSignal::new(80.0);
    let panel_width: RwSignal<f64> = RwSignal::new(520.0);
    let panel_height: RwSignal<f64> = RwSignal::new(480.0);
    // 拖拽状态：抓取时记下鼠标相对面板左上角的偏移，松手时清空。
    let dragging: RwSignal<bool> = RwSignal::new(false);
    let drag_origin: RwSignal<(f64, f64)> = RwSignal::new((0.0, 0.0));
    // 缩放状态：抓取时记下 (起始鼠标 x/y, 起始面板 w/h)，松手时按
    // 鼠标位移 + 起始尺寸算出新尺寸并夹到上下界。
    let resizing: RwSignal<bool> = RwSignal::new(false);
    let resize_origin: RwSignal<(f64, f64, f64, f64)> = RwSignal::new((0.0, 0.0, 0.0, 0.0));
    // 面板尺寸上下界。
    const PANEL_MIN_W: f64 = 320.0;
    const PANEL_MIN_H: f64 = 320.0;
    const PANEL_DEF_W: f64 = 520.0;
    const PANEL_DEF_H: f64 = 480.0;

    // 第一次打开时定位：优先 localStorage 落值，否则按视口钉到右下角并保留 10px 边距。
    // Effect 只在 open 翻 true 的瞬间跑一次（依赖里只放 open），后续拖拽 / 缩放
    // 不会重新触发——避免把用户调好的位置 / 尺寸又重置成默认。
    Effect::new(move |_| {
        if !open.0.get() {
            return;
        }
        if let Some((l, t, w, h)) = load_panel_layout() {
            panel_left.set(l);
            panel_top.set(t);
            panel_width.set(w.max(PANEL_MIN_W));
            panel_height.set(h.max(PANEL_MIN_H));
            return;
        }
        if let Some((vw, vh)) = viewport_size() {
            panel_width.set(PANEL_DEF_W);
            panel_height.set(PANEL_DEF_H);
            panel_left.set((vw - PANEL_DEF_W - 10.0).max(10.0));
            panel_top.set((vh - PANEL_DEF_H - 10.0).max(60.0));
        }
    });

    // 消息列表自动滚动：贴底时把 scroll_top 推到 scroll_height。
    // 仅 wasm32 走——native / SSR 路径没有 web_sys，且 messages_ref.get() 在 SSR 时为 None。
    // 不引入 request_animation_frame：leptos 的 Effect::new 默认就在 render 之后跑，
    // 此时 `.agent-messages` 子节点已经更新，scroll_height 反映最新 DOM。
    #[cfg(target_arch = "wasm32")]
    Effect::new(move |_| {
        // 跟踪 messages 变化；streaming 不直接进这里（delta 也走 messages.update）。
        let _ = messages.get();
        if !messages_at_bottom.get_untracked() {
            return;
        }
        if let Some(el) = messages_ref.get() {
            el.set_scroll_top(el.scroll_height());
        }
    });

    // 头部拖拽起点：仅 mousedown 时启用 cursor:move，移动 / 松手由窗口级
    // 监听兜底——这样即便鼠标快速拖出 header 也跟得住。ev.prevent_default()
    // 拦掉文本选区：拖拽过程中如果浏览器默认选中「Buckaroo」等文字，松手后会
    // 留下脏选中态。
    //
    // 点击发生在 × 关闭按钮 / + 新建按钮 时不要拖：这两个按钮自带 click handler，
    // 触发 drag 会让面板跟手、按钮原地，体感是「点了按钮结果整个面板飞走」。
    // native / SSR 路径下没有 web_sys，整段类名判定仅 wasm32 走。
    let start_drag = move |ev: leptos::ev::MouseEvent| {
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsCast;
            if let Some(target) = ev.target() {
                if let Some(node) = target.dyn_ref::<web_sys::Element>() {
                    let cls = node.class_name();
                    if cls.contains("agent-new") || cls.contains("agent-close") {
                        return;
                    }
                }
            }
        }
        ev.prevent_default();
        dragging.set(true);
        drag_origin.set((ev.client_x() as f64 - panel_left.get_untracked(),
                         ev.client_y() as f64 - panel_top.get_untracked()));
    };
    // 右下角缩放起点：与 start_drag 同款约定，但起点记的是鼠标坐标和面板
    // 当前尺寸，松手时按位移增量算新尺寸。
    let start_resize = move |ev: leptos::ev::MouseEvent| {
        ev.prevent_default();
        ev.stop_propagation();
        resizing.set(true);
        resize_origin.set((ev.client_x() as f64,
                           ev.client_y() as f64,
                           panel_width.get_untracked(),
                           panel_height.get_untracked()));
    };

    // 窗口级 mousemove / mouseup：跟手移动 + 释放时落 localStorage。
    // 限制只能挂在 wasm32：native target 下没有 window_event_listener；
    // SSR 路径上也用不到这些状态。
    if cfg!(target_arch = "wasm32") {
        let handle = window_event_listener(leptos::ev::mousemove, move |ev| {
            if dragging.get_untracked() {
                let (ox, oy) = drag_origin.get_untracked();
                let mut l = ev.client_x() as f64 - ox;
                let mut t = ev.client_y() as f64 - oy;
                // 视口内可夹一道 8px 边距：避免拖出屏幕后找不到。完整
                // clamp 会让面板被视口大小限制，体感「到边就停」够用。
                if let Some((vw, vh)) = viewport_size() {
                    let w = panel_width.get_untracked();
                    let h = panel_height.get_untracked();
                    l = l.max(8.0).min((vw - 40.0).max(8.0));
                    t = t.max(8.0).min((vh - 40.0).max(8.0));
                    // 保证 h 那一行不超出：单独窗口高度小于 panel_h 时不强求整个
                    // 面板可见——顶栏 50px 一并预留。
                    let _ = (w, h);
                }
                panel_left.set(l);
                panel_top.set(t);
            } else if resizing.get_untracked() {
                let (sx, sy, sw, sh) = resize_origin.get_untracked();
                let mut nw = sw + (ev.client_x() as f64 - sx);
                let mut nh = sh + (ev.client_y() as f64 - sy);
                nw = nw.max(PANEL_MIN_W);
                nh = nh.max(PANEL_MIN_H);
                if let Some((vw, vh)) = viewport_size() {
                    nw = nw.min(vw * 0.95);
                    nh = nh.min(vh * 0.92);
                }
                panel_width.set(nw);
                panel_height.set(nh);
            }
        });
        on_cleanup(move || handle.remove());

        let handle2 = window_event_listener(leptos::ev::mouseup, move |_ev| {
            let was_active = dragging.get_untracked() || resizing.get_untracked();
            dragging.set(false);
            resizing.set(false);
            if was_active {
                save_panel_layout(
                    panel_left.get_untracked(),
                    panel_top.get_untracked(),
                    panel_width.get_untracked(),
                    panel_height.get_untracked(),
                );
            }
        });
        on_cleanup(move || handle2.remove());
    }

    // 加载会话列表：open 打开 + workspace_id 就绪时拉一次；切换 workspace 时
    // 也跟着重拉（Resource 自动追踪 key 变化）。
    // 没有当前会话时自动选最近一条——面板一打开就处于「可发消息」状态，
    // 否则老用户带着一堆历史会话进来，send 按钮亮着但 current_session 还是
    // None，消息发出去会被 on_send 早返回 + 仅 console.warn 静默丢掉。
    Effect::new(move |_| {
        let is_open = open.0.get();
        let ws_id = workspace_id_resolved.get();
        if !is_open {
            return;
        }
        let Some(ws_id) = ws_id else { return };
        spawn_local(async move {
            match list_agent_sessions(&ws_id).await {
                Ok(list) => {
                    sessions.set(list);
                    if current_session.get_untracked().is_none() {
                        if let Some(first) = sessions.get_untracked().first() {
                            current_session.set(Some(first.id.clone()));
                        }
                    }
                }
                Err(e) => web_sys_console_warn(&format!("list_agent_sessions failed: {e}")),
            }
        });
    });

    // 当前会话切换 → 拉消息历史。
    Effect::new(move |_| {
        let sid = current_session.get();
        spawn_local(async move {
            match sid {
                Some(s) => match list_agent_messages(&s).await {
                    Ok(msgs) => messages.set(msgs),
                    Err(e) => web_sys_console_warn(&format!("list_agent_messages failed: {e}")),
                },
                None => messages.set(Vec::new()),
            }
        });
        // 切会话时把上一会话的附件 chip 清空——chip 里只有 id,落到新会话
        // 上 send 也会被服务端当作新会话的附件,但语义跟用户预期不一致。
        attachments.set(Vec::new());
        attachments_error.set(None);
        // 切会话时同步清空流式 ToolCall 卡片——避免上一会话未结束的乐观态
        // 跑到新会话里,触发「这是哪个工具的卡片」的混淆。
        streaming_tool_calls.set(Vec::new());
    });

    // 参数显式标 `()`：原来 `move |_|` 是让 on:click 推断成 MouseEvent handler,
    // 但 keydown 里需要直接调 `on_send()`——同一闭包既要做事件 handler 又要
    // 显式调用,_ 不能两边都推断。改成 `()` 后,on:click 走一层薄包装忽略
    // MouseEvent 参数,on:keydown 直接调,两条路径都收敛到同一段逻辑。
    //
    // 拆成 do_send + on_send 两层：do_send 假设 sid 已经就绪（直接乐观写、
    // 调 sendAgentMessage），on_send 在没 sid 时先 create_agent_session 再
    // 调 do_send。原来的「没 sid 就 console.warn + return」会让首次进来的
    // 用户打了一行字按 Enter 静默丢掉、session 导航栏永远空——见 2026-09-29 缺陷 #2。
    let do_send = move |sid: String, content: String, ws_id: String| {
        if streaming.get_untracked() {
            return;
        }
        // 乐观写入用户消息：占位 id 用 `local-{ms}`，等 SSE done 后会调
        // list_agent_messages 把整列替换成真消息（ULID）—— 占位 id 自然消失。
        // 这里不再 reconcile id，而是依赖 done 后的整体重拉（见 open_stream 末尾）。
        // 生成一次 id 并捕获，send / SSE 任一路径失败时立即 pop 掉占位——
        // 不让用户看见「我发了但服务端没收」的不一致状态。
        let placeholder_id = ulid_compat_id();
        messages.update(|m| {
            m.push(AgentMessage {
                id: placeholder_id.clone(),
                session_id: sid.clone(),
                role: "USER".to_string(),
                content: content.clone(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: String::new(),
            });
        });
        input.set(String::new());
        // 已选附件（id 来自 uploadAttachment）按顺序传给 sendAgentMessage。
        // 同时清空 chip 列表——发完即用完。
        let attachment_ids: Vec<String> = attachments
            .get_untracked()
            .into_iter()
            .map(|a| a.id)
            .collect();
        attachments.set(Vec::new());
        attachments_error.set(None);
        // 视图 + 选中条目上下文：闭包快照。拿不到 ctx 的路由（登录 / 账号页）发 None。
        let context_md = agent_ctx.map(|c| AgentMessageContext {
            view_id: c.view_id.get_untracked(),
            selected_entry_codes: c.selected_entry_codes.get_untracked(),
        });
        streaming.set(true);
        let sid_for_refresh = sid.clone();
        let attachment_ids_for_send = attachment_ids.clone();
        // 服务端 send_agent_message 在落 user 消息后会同步写一个 title（首条
        // user 消息的截断文本）。这里同步更新本地 sessions 缓存，省一次
        // list_agent_sessions 重拉——`derive_session_title` 是对应的本地算法，
        // 见 helpers 区注释。
        if let Some(derived) = local_session_title(&content) {
            sessions.update(|l| {
                if let Some(s) = l.iter_mut().find(|s| s.id == sid) {
                    if s.title.is_empty() {
                        s.title = derived.clone();
                    }
                }
            });
        }
        spawn_local(async move {
            match send_agent_message(
                &sid,
                &content,
                context_md,
                if attachment_ids_for_send.is_empty() { None } else { Some(attachment_ids_for_send) },
            ).await {
                Ok(turn) => {
                    // 占位自然由 open_stream 之后的 post-stream list_agent_messages
                    // 替换为真 ULID 消息——SSE 路径出错也不需要就地清，因为
                    // 服务端已经把 user_msg 落库（graphql.rs send_agent_message
                    // 先 append 再 run_turn），list_agent_messages 能拿回真消息。
                    open_stream(&turn.id, messages, streaming, streaming_tool_calls, effects).await;
                }
                Err(e) => {
                    streaming.set(false);
                    // send 直接挂了服务端也没收到，乐观写入的占位要就地 pop——
                    // 等下面的 post-stream list_agent_messages 兜底太慢，而且
                    // 网络一起挂的话根本不会有响应。
                    messages.update(|m| m.retain(|x| x.id != placeholder_id));
                    web_sys_console_warn(&format!("send_agent_message failed: {e}"));
                }
            }
            // SSE 完成（无论正常 done 还是失败退出）后从服务端拉真消息列表：
            // 这样乐观写入的 `local-{ms}` 占位被 ULID 真消息替换，且 tool_call /
            // tool_result 之类服务端追加的事件流一并回来——前端不必靠 SSE 累积
            // 出全部状态。
            //
            // 守卫：用户中途切到别的会话就不要写回——Effect 已经在 current_session
            // 变化时拉过新会话的消息，这里再 set 会把新会话刷成旧的。
            if current_session.get_untracked().as_deref() == Some(sid_for_refresh.as_str()) {
                match list_agent_messages(&sid_for_refresh).await {
                    Ok(real) => messages.set(real),
                    Err(e) => web_sys_console_warn(&format!(
                        "post-stream list_agent_messages failed: {e}"
                    )),
                }
            }
        });
        // ws_id 暂时只在「新建会话」按钮里用；保留变量让编译器不报警告。
        let _ = ws_id;
    };

    let on_send = move || {
        // 闭包不在 setup 段；`.get_untracked()` 拿调用时刻的快照即可——
        // send 后 streaming / input 的变化不需要回头更新这个回调。
        let content = input.get_untracked();
        if content.trim().is_empty() {
            return;
        }
        let Some(ws_id) = workspace_id_resolved.get_untracked() else {
            web_sys_console_warn("workspace not ready");
            return;
        };
        if streaming.get_untracked() {
            return;
        }
        if let Some(sid) = current_session.get_untracked() {
            do_send(sid, content, ws_id);
        } else {
            // 没选会话：自动建一个再走 do_send。建失败时 input 内容保留，
            // 用户能看到自己刚才打了什么、可以重试或去点「+ 新建」。
            spawn_local(async move {
                match create_agent_session(&ws_id).await {
                    Ok(s) => {
                        sessions.update(|l| l.insert(0, s.clone()));
                        current_session.set(Some(s.id.clone()));
                        do_send(s.id, content, ws_id);
                    }
                    Err(e) => {
                        web_sys_console_warn(&format!(
                            "create_agent_session (auto from send) failed: {e}"
                        ));
                    }
                }
            });
        }
    };

    let on_new_session = move |_| {
        let Some(ws_id) = workspace_id_resolved.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match create_agent_session(&ws_id).await {
                Ok(s) => {
                    sessions.update(|l| l.insert(0, s.clone()));
                    current_session.set(Some(s.id));
                }
                Err(e) => web_sys_console_warn(&format!("create_agent_session failed: {e}")),
            }
        });
    };

    // 选完文件即传：每个文件都跑一次 uploadAttachment，agent_session_id
    // 钉到当前会话。失败文件不进 attachments、提示挂在 attachments_error。
    // 多文件并发——单文件上限 50 MiB，整体上传耗时只受最大单文件影响。
    let on_pick_files = move |ev: leptos::ev::Event| {
        spawn_attachment_uploads(
            ev,
            current_session.get_untracked(),
            attachments,
            attachments_uploading,
            attachments_error,
        );
    };

    view! {
        <Show when=move || open.0.get()>
            <div
                class="agent-panel"
                style=move || format!(
                    "left:{}px;top:{}px;width:{}px;height:{}px;",
                    panel_left.get(), panel_top.get(),
                    panel_width.get(), panel_height.get(),
                )
            >
                <header class="agent-header" on:mousedown=start_drag>
                    // 头像：内联 SVG，不走 extra 网络请求；语义上呼应「Rodeo」——
                    // 牛仔帽是西部世界的最简符号。`currentColor` 让色块跟随
                    // agent-header 的 color，主题色翻转时无需另存一份图。
                    <span class="agent-avatar" aria-hidden="true">
                        <svg viewBox="0 0 32 32" xmlns="http://www.w3.org/2000/svg">
                            <ellipse cx="16" cy="22" rx="13" ry="2.2" fill="currentColor"/>
                            <path d="M 9 12 Q 9 8, 16 8 Q 23 8, 23 12 L 23 20 L 9 20 Z" fill="currentColor"/>
                            <rect x="9" y="18" width="14" height="1.6" fill="currentColor"/>
                            <rect x="9" y="20" width="14" height="1.4" fill="currentColor" opacity="0.45"/>
                        </svg>
                    </span>
                    <span class="agent-title">"Buckaroo"</span>
                    <button class="agent-new" on:click=on_new_session>"+ 新建"</button>
                    <button class="agent-close" on:click=move |_| open.0.set(false)>"×"</button>
                </header>
                <Suspense fallback=move || view! { <div class="agent-loading">"加载中…"</div> }>
                    {move || match workspace_id_resolved.get() {
                        None => view! { <div class="agent-loading">"加载中…"</div> }.into_any(),
                        Some(_) => view! {
                            <div class="agent-body">
                                <aside class="agent-sessions">
                                    <For
                                        each=move || sessions.get()
                                        key=|s| s.id.clone()
                                        children=move |s: AgentSession| {
                                            let sid_for_click = s.id.clone();
                                            let sid_for_active = s.id.clone();
                                            let sid_for_delete = s.id.clone();
                                            let on_delete = move |ev: leptos::ev::MouseEvent| {
                                                // 别让删除按钮的点击冒泡到外层 item 的
                                                // on:click —— 那个 handler 会把会话
                                                // 切到一个即将被删的会话上，触发空
                                                // 消息列表 + 下一次发消息时切回再
                                                // 错位。stop_propagation 后删除只走
                                                // 这条路径。
                                                ev.stop_propagation();
                                                let sid = sid_for_delete.clone();
                                                spawn_local(async move {
                                                    match delete_agent_session(&sid).await {
                                                        Ok(_) => {
                                                            sessions.update(|l| {
                                                                l.retain(|x| x.id != sid)
                                                            });
                                                            if current_session
                                                                .get_untracked()
                                                                .as_deref()
                                                                == Some(sid.as_str())
                                                            {
                                                                // 删的是当前会话：直接跳到
                                                                // 列表里第一条（仍按更新时间
                                                                // 倒序），而不是退回到「未选」
                                                                // ——后者会让用户必须再点一
                                                                // 下才能继续发。
                                                                if let Some(next) =
                                                                    sessions.get_untracked().first()
                                                                {
                                                                    current_session
                                                                        .set(Some(next.id.clone()));
                                                                } else {
                                                                    current_session.set(None);
                                                                }
                                                            }
                                                        }
                                                        Err(e) => web_sys_console_warn(&format!(
                                                            "delete_agent_session failed: {e}"
                                                        )),
                                                    }
                                                });
                                            };
                                            view! {
                                                <div
                                                    class="agent-session-item"
                                                    class:agent-session-active=move || current_session.get().as_deref() == Some(sid_for_active.as_str())
                                                    on:click={
                                                        let sid = sid_for_click;
                                                        move |_| current_session.set(Some(sid.clone()))
                                                    }
                                                >
                                                    // 新建会话时 title 为空串，没东西显示就成
                                                    // 「看不见条目」。补一个占位——日期比
                                                    // 编号稳（同日多次新建不至于全撞 "新会话"），
                                                    // 让用户能区分多条空标题会话。
                                                    <span class="agent-session-title">{session_label(&s)}</span>
                                                    <button
                                                        class="agent-session-del"
                                                        title="删除会话"
                                                        on:click=on_delete
                                                    >"×"</button>
                                                </div>
                                            }
                                        }
                                    />
                                </aside>
                                <main class="agent-chat">
                                    <div
                                        class="agent-messages"
                                        node_ref=messages_ref
                                        on:scroll=move |_ev: leptos::ev::Event| {
                                            // 滚动事件触发时更新「贴底」判据。wasm32 only——
                                            // SSR / native 不会发出 Event，参数与函数体整段关掉。
                                            #[cfg(target_arch = "wasm32")]
                                            {
                                                use wasm_bindgen::JsCast;
                                                let target = _ev.target().unwrap();
                                                let el: &web_sys::Element = target.unchecked_ref();
                                                let top = el.scroll_top() as f64;
                                                let height = el.scroll_height() as f64;
                                                let client = el.client_height() as f64;
                                                messages_at_bottom.set(height - top - client < 32.0);
                                            }
                                        }
                                    >
                                        <For
                                            each=move || messages.get()
                                            key=|m| m.id.clone()
                                            children=|m: AgentMessage| {
                                                let content_html = render_markdown(&m.content);
                                                let role_cls = format!(
                                                    "agent-msg agent-msg-{}",
                                                    m.role.to_lowercase()
                                                );
                                                // 推理链：仅前端流式期间累积,服务端不持久化。
                                                // 渲染成可折叠 <details>——典型情况(DeepSeek / o1)
                                                // 推理几百字到几千字,默认收起避免撑爆聊天区;
                                                // 用户主动点开看思考细节。
                                                let reasoning_html = render_markdown(&m.reasoning);
                                                let has_reasoning = !m.reasoning.is_empty();
                                                // tool_calls 折叠：args + result_preview 渲染进 <details>。
                                                // 流式占位（role=TOOL_CALL / TOOL_RESULT_*）没有 tool_calls
                                                // 字段，Vec::new() 自然零次循环——只渲染 content 文字。
                                                // server 重拉后的真消息里 `args` 是 serde_json::Value，
                                                // to_string 出来已经是合法 JSON 文本（数组 / 对象均行）。
                                                let tool_calls = m.tool_calls.clone();
                                                view! {
                                                    <div class=role_cls>
                                                        <Show
                                                            when=move || has_reasoning
                                                            fallback=move || view! { <span></span> }
                                                        >
                                                            <details class="agent-think">
                                                                <summary>"思考过程"</summary>
                                                                <div class="agent-think-body" inner_html=reasoning_html.clone()></div>
                                                            </details>
                                                        </Show>
                                                        <div class="agent-msg-content" inner_html=content_html></div>
                                                        <For
                                                            each=move || tool_calls.clone()
                                                            key=|tc| tc.id.clone()
                                                            children=|tc: crate::frontend::graphql_client::AgentToolCallRecord| {
                                                                let name = tc.name.clone();
                                                                let args_text = serde_json::to_string_pretty(&tc.args)
                                                                    .unwrap_or_else(|_| tc.args.to_string());
                                                                // 截断 result_preview：折叠态只露前 N 字符，避免详情块一展开就被
                                                                // 几百行 JSON 撑爆整个聊天区；点开 <details> 看全文。
                                                                let preview_truncated: String = tc.result_preview.chars().take(280).collect();
                                                                let preview_full = tc.result_preview.clone();
                                                                let ok = tc.ok;
                                                                let detail_id = format!("agent-tool-{}", tc.id);
                                                                view! {
                                                                    <details class="agent-tool" id=detail_id>
                                                                        <summary>
                                                                            <span class="agent-tool-icon" aria-hidden="true">{if ok { "✓" } else { "✗" }}</span>
                                                                            <span class="agent-tool-name">{name}</span>
                                                                        </summary>
                                                                        <div class="agent-tool-body">
                                                                            <div class="agent-tool-label">"args"</div>
                                                                            <pre class="agent-tool-args">{args_text}</pre>
                                                                            <div class="agent-tool-label">"result"</div>
                                                                            <pre class=if ok { "agent-tool-result" } else { "agent-tool-result err" }>{preview_truncated}</pre>
                                                                            // 截断后才把全文写进一个隐藏的 <pre>，由 CSS 在 details[open]
                                                                            // 时把它显示出来——避免 SSE 期间重复渲染几百行字符串。
                                                                            <details class="agent-tool-result-full">
                                                                                <summary>"查看完整 result"</summary>
                                                                                <pre>{preview_full}</pre>
                                                                            </details>
                                                                        </div>
                                                                    </details>
                                                                }
                                                            }
                                                        />
                                                    </div>
                                                }
                                            }
                                        />
                                        // 流式 ToolCall 卡片：tool_call / tool_result
                                        // 事件累积进 streaming_tool_calls；这里取前 4
                                        // 个并排 grid，5+ 进 <details> 折叠。
                                        // 仅在 streaming_tool_calls 非空时挂卸——空状态
                                        // 不占布局。list_agent_messages 拉到真消息后
                                        // 上面的 <For> 会渲染 m.tool_calls（结构化、
                                        // 完整配对），本块在 done 事件被清空。
                                        {move || {
                                            let tcs = streaming_tool_calls.get();
                                            if tcs.is_empty() {
                                                return view! { <span></span> }.into_any();
                                            }
                                            let total = tcs.len();
                                            let hidden = total.saturating_sub(4);
                                            view! {
                                                <div class="agent-msg agent-msg-tool_call">
                                                    <div class="tool-call-grid">
                                                        <For
                                                            each=move || {
                                                                streaming_tool_calls
                                                                    .get()
                                                                    .into_iter()
                                                                    .take(4)
                                                                    .collect::<Vec<_>>()
                                                            }
                                                            key=|tc| tc.id.clone()
                                                            children=|tc: ToolCallDisplay| {
                                                                let icon = match tc.status {
                                                                    ToolCallStatus::Running => "…",
                                                                    ToolCallStatus::Ok => "✓",
                                                                    ToolCallStatus::Err => "✗",
                                                                };
                                                                let cls = match tc.status {
                                                                    ToolCallStatus::Running => "tool-call-card tool-call-running",
                                                                    ToolCallStatus::Ok => "tool-call-card tool-call-ok",
                                                                    ToolCallStatus::Err => "tool-call-card tool-call-err",
                                                                };
                                                                let name = tc.name.clone();
                                                                view! {
                                                                    <div class=cls>
                                                                        <span class="tool-call-icon" aria-hidden="true">{icon}</span>
                                                                        <span class="tool-call-name">{name}</span>
                                                                    </div>
                                                                }
                                                            }
                                                        />
                                                    </div>
                                                    <Show
                                                        when=move || { streaming_tool_calls.get().len() >= 5 }
                                                        fallback=move || view! { <span></span> }
                                                    >
                                                        <details class="tool-call-overflow">
                                                            <summary>{format!("{hidden} more")}</summary>
                                                            <div class="tool-call-grid">
                                                                <For
                                                                    each=move || {
                                                                        streaming_tool_calls
                                                                            .get()
                                                                            .into_iter()
                                                                            .skip(4)
                                                                            .collect::<Vec<_>>()
                                                                    }
                                                                    key=|tc| tc.id.clone()
                                                                    children=|tc: ToolCallDisplay| {
                                                                        let icon = match tc.status {
                                                                            ToolCallStatus::Running => "…",
                                                                            ToolCallStatus::Ok => "✓",
                                                                            ToolCallStatus::Err => "✗",
                                                                        };
                                                                        let cls = match tc.status {
                                                                            ToolCallStatus::Running => "tool-call-card tool-call-running",
                                                                            ToolCallStatus::Ok => "tool-call-card tool-call-ok",
                                                                            ToolCallStatus::Err => "tool-call-card tool-call-err",
                                                                        };
                                                                        let name = tc.name.clone();
                                                                        view! {
                                                                            <div class=cls>
                                                                                <span class="tool-call-icon" aria-hidden="true">{icon}</span>
                                                                                <span class="tool-call-name">{name}</span>
                                                                            </div>
                                                                        }
                                                                    }
                                                                />
                                                            </div>
                                                        </details>
                                                    </Show>
                                                </div>
                                            }.into_any()
                                        }}
                                    </div>
                                    <div class="agent-input">
                                        <div class="agent-attachments-bar">
                                            <label class="agent-attach-pick">
                                                {move || if attachments_uploading.get() {
                                                    "上传中…"
                                                } else {
                                                    "📎 附件"
                                                }}
                                                <input
                                                    type="file"
                                                    multiple=true
                                                    style="display:none"
                                                    prop:disabled=move || attachments_uploading.get() || streaming.get()
                                                    on:change=on_pick_files
                                                />
                                            </label>
                                            // 已选附件 chip 列表：点击 × 把对应附件从 attachments 里剔掉。
                                            // 这里没用 <For> key 索引 id——ids 是 ULID，重复概率极低，
                                            // 且渲染顺序就是添加顺序，键稳定。
                                            {move || {
                                                let chips = attachments.get();
                                                if chips.is_empty() {
                                                    view! { <span></span> }.into_any()
                                                } else {
                                                    view! {
                                                        <div class="agent-attach-chips">
                                                            {chips.into_iter().map(|chip| {
                                                                let remove_id = chip.id.clone();
                                                                let name = chip.filename.clone();
                                                                let size = human_size(chip.size);
                                                                view! {
                                                                    <span class="agent-attach-chip">
                                                                        <span class="agent-attach-chip-name">{name}</span>
                                                                        <span class="agent-attach-chip-size">{size}</span>
                                                                        <button
                                                                            class="agent-attach-chip-x"
                                                                            title="移除"
                                                                            on:click={
                                                                                let rid = remove_id.clone();
                                                                                move |_| {
                                                                                    attachments.update(|l| {
                                                                                        l.retain(|c| c.id != rid)
                                                                                    });
                                                                                }
                                                                            }
                                                                        >"×"</button>
                                                                    </span>
                                                                }
                                                            }).collect::<Vec<_>>()}
                                                        </div>
                                                    }.into_any()
                                                }
                                            }}
                                        </div>
                                        <Show
                                            when=move || attachments_error.get().is_some()
                                            fallback=move || view! { <span></span> }
                                        >
                                            <p class="agent-att-warn">
                                                {move || attachments_error.get().unwrap_or_default()}
                                            </p>
                                        </Show>
                                        <textarea
                                            class="agent-textarea"
                                            bind:value=input
                                            prop:disabled=move || streaming.get()
                                            placeholder="向 Buckaroo 提问…回车发送，Ctrl/⌘+回车换行"
                                            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                                                // 回车直接发送：与主流 IM / 聊天面板一致;
                                                // Shift+Enter / Ctrl+Enter / ⌘+Enter 留给浏览器默认
                                                // （插入换行）——避免与全局快捷键（Ctrl+Enter 也
                                                // 经常被绑成「换行」以外的语义）冲突。
                                                // 输入期间按下时 textarea 仍是焦点元素,
                                                // prevent_default 阻止浏览器在按 Enter 时
                                                // 默认的「插入换行」生效——on_send 触发的发送流程
                                                // 负责清空 textarea。
                                                if ev.key() == "Enter"
                                                    && !ev.shift_key()
                                                    && !ev.ctrl_key()
                                                    && !ev.meta_key()
                                                {
                                                    ev.prevent_default();
                                                    on_send();
                                                }
                                            }
                                        />
                                        <button
                                            class="agent-send"
                                            on:click=move |_| on_send()
                                            prop:disabled=move || streaming.get() || attachments_uploading.get()
                                        >
                                            {move || if streaming.get() { "..." } else { "发送" }}
                                        </button>
                                    </div>
                                    // 流式期间在输入区下方挂一条 status：spinner + 文案
                                    // 「Buckaroo 正在思考…」——让用户看见 agent 确实在跑，
                                    // 不是按了发送毫无反馈。SSE 结束（成功或失败）后 streaming
                                    // 翻 false，块整体卸载。
                                    <Show
                                        when=move || streaming.get()
                                        fallback=move || view! { <span></span> }
                                    >
                                        <div class="agent-streaming" aria-live="polite">
                                            <span class="agent-spinner" aria-hidden="true"></span>
                                            <span class="agent-streaming-text">"Buckaroo 正在思考…"</span>
                                        </div>
                                    </Show>
                                </main>
                            </div>
                        }.into_any(),
                    }}
                </Suspense>
                // 右下角缩放手柄：12x12 三角形 + cursor:nwse-resize。
                // mousedown 在手柄上启用 resizing，mousemove/mouseup 由
                // 窗口级监听统一驱动，这样即便把鼠标拖到手柄外也跟得住。
                <div class="agent-resize-handle" on:mousedown=start_resize aria-hidden="true"></div>
            </div>
        </Show>
    }
}

/// 占位的 id 生成器。前端给乐观写入的消息一个本地点 id，让 `<For key>`
/// 稳定。`list_agent_messages` 重新拉到真消息后这条会被自然替换掉。
///
/// 必须是 wasm32-safe：之前用 `SystemTime::now()` 在 hydrate 后会 panic
///（wasm32 的 `std::time` 后端是 `unsupported.rs`，`time not implemented on
/// this platform`），正好踩在 `on_send` 里乐观写入之前——结果用户消息
/// 永远推不进 `messages` 信号。改用进程级单调计数器，跨刷新不需要持久
///（占位只在客户端活到 SSE 之后的 list_agent_messages 覆盖为止）。
fn ulid_compat_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("local-{n}")
}

/// HTML escape：把 `&` / `<` / `>` / `"` / `'` 转成 entities。
/// agent 输出的所有内容（assistant delta 累积、tool result 预览、用户
/// 输入、错误消息）都过这一道再拼到 `inner_html` 里，避免 XSS / DOM 注入。
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// URL 安全检查：只放行 http / https / mailto，其它（`javascript:` /
/// `data:` / `vbscript:` 等）原样返回空串。LLM 不太可能产这些，但
/// `inner_html` 的信任面要窄。
fn safe_url(u: &str) -> Option<String> {
    let t = u.trim_start();
    if t.starts_with("http://") || t.starts_with("https://") || t.starts_with("mailto:") {
        Some(t.to_string())
    } else {
        None
    }
}

/// 把一段 Markdown 文本转成 HTML。覆盖范围：```代码块```、行内 `code`、
/// `**bold**`、`[label](url)` 链接。HTML 字符全量 escape，URL 过
/// `safe_url` 过滤。
///
/// 故意只做这一小撮：agent 系统提示（service/ai.rs）要求结构化 Markdown，
/// 实际输出以「短文 + 偶尔一段代码块 + 偶尔一两个链接」为主，嵌套列表 /
/// 引用 / 表格在 agent 上下文里几乎不会出现，靠 tiny-editor 输入那边
/// 有完整 Quill 支持。代码块是最大价值——LLM 给的 JSON / SQL 示例在
/// `<pre>` 里会跑成一坨，换 `<pre><code>` 至少能等宽显示。
///
/// 处理流程（按段切）：
/// 1. 用 ``` 三引号切分输入，奇数段是代码块（escape + 套 `<pre><code>`），
///    偶数段是普通文本（escape 后跑行内替换 + `\n → <br>`）。
/// 2. 行内替换：行内 `code`、`**bold**`、`[label](url)`，均在 escaped
///    字符串上做（`*` `[` `]` `(` `)` 都不是 HTML 字符，不会有冲突）。
fn render_markdown(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    let mut in_code = false;
    loop {
        // 找下一个 ``` 的位置；找不到就把剩余全部按当前模式收尾。
        let fence_idx = rest.find("```");
        match (fence_idx, in_code) {
            (Some(idx), false) => {
                // 普通文本段：[..idx) → escape + 行内 + \n → <br>
                let segment = &rest[..idx];
                out.push_str(&render_text_segment(segment));
                rest = &rest[idx + 3..];
                in_code = true;
            }
            (Some(idx), true) => {
                // 代码块段：[..idx) → escape + 套 <pre><code>
                let code = &rest[..idx];
                out.push_str("<pre><code>");
                out.push_str(&html_escape(code));
                out.push_str("</code></pre>");
                rest = &rest[idx + 3..];
                in_code = false;
            }
            (None, false) => {
                out.push_str(&render_text_segment(rest));
                break;
            }
            (None, true) => {
                // 孤立的 ``` 没有收尾：剩余按普通文本走（避免吞到结尾）。
                out.push_str(&render_text_segment(rest));
                break;
            }
        }
    }
    out
}

/// 一段非代码文本：escape 后做行内替换 + `\n → <br>`。
fn render_text_segment(s: &str) -> String {
    let escaped = html_escape(s);
    let with_inline = render_inline(&escaped);
    // escaped 之后只剩换行是真实字符；每个 `\n` 换 `<br>`，连续 `\n` 产生
    // 多个 `<br>` 形成段落间距（不另加 `<p>`：CSS 自己控 line-height 更简单）。
    let mut out = String::with_capacity(with_inline.len());
    let mut first = true;
    for part in with_inline.split('\n') {
        if !first {
            out.push_str("<br>");
        }
        out.push_str(part);
        first = false;
    }
    out
}

/// 行内替换：`**bold**` → `<strong>...</strong>`，
/// `` `code` `` → `<code>...</code>`，`[label](url)` → `<a>...</a>`。
///
/// 只识别「前后都不是字母数字下划线」的 `**`，避免吞到单词里；行内
/// `code` 要求前后不是反引号。链接 URL 过 `safe_url`。
fn render_inline(s: &str) -> String {
    // 用字节索引扫描 + String replace。`s` 已经被 html_escape 过一次，
    // `&` `<` `>` `"` `'` 都是 entities，不会与 markdown 标记冲突；
    // `*` `[` `]` `(` `)` 都是 ASCII 单字节，可以按 byte 位置切。
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // 行内 `code`
        if b == b'`' && !starts_with(&bytes[i + 1..], b"`") {
            // 找下一个 `
            if let Some(end_rel) = find_byte(&bytes[i + 1..], b'`') {
                let inner = &s[i + 1..i + 1 + end_rel];
                out.push_str("<code>");
                out.push_str(inner);
                out.push_str("</code>");
                i = i + 1 + end_rel + 1;
                continue;
            }
        }
        // **bold**
        if b == b'*' && bytes.get(i + 1) == Some(&b'*') {
            // open `**` 前不是 word
            let safe_open = i == 0 || !is_word_byte(bytes[i - 1]);
            if safe_open {
                let after_open = i + 2;
                if let Some(close_rel) = find_subslice(&bytes[after_open..], b"**") {
                    let close = after_open + close_rel;
                    let safe_close = close + 2 >= bytes.len() || !is_word_byte(bytes[close + 2]);
                    if safe_close {
                        let inner = &s[after_open..close];
                        out.push_str("<strong>");
                        out.push_str(inner);
                        out.push_str("</strong>");
                        i = close + 2;
                        continue;
                    }
                }
            }
        }
        // [label](url)
        if b == b'[' {
            if let Some(rb_rel) = find_byte(&bytes[i + 1..], b']') {
                let rb = i + 1 + rb_rel;
                let after_rb = rb + 1;
                if after_rb < bytes.len() && bytes[after_rb] == b'(' {
                    if let Some(rp_rel) = find_byte(&bytes[after_rb + 1..], b')') {
                        let rp = after_rb + 1 + rp_rel;
                        let label = &s[i + 1..rb];
                        let url_raw = &s[after_rb + 1..rp];
                        if let Some(url) = safe_url(url_raw) {
                            out.push_str("<a href=\"");
                            out.push_str(&url);
                            out.push_str("\" target=\"_blank\" rel=\"noopener\">");
                            out.push_str(label);
                            out.push_str("</a>");
                            i = rp + 1;
                            continue;
                        }
                    }
                }
            }
        }
        // 普通字符（含 UTF-8 多字节序列的开头）：拷一个字符。
        let ch_end = next_char_boundary(s, i);
        out.push_str(&s[i..ch_end]);
        i = ch_end;
    }
    out
}

fn find_byte(hay: &[u8], needle: u8) -> Option<usize> {
    hay.iter().position(|&b| b == needle)
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn starts_with(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() >= needle.len() && &hay[..needle.len()] == needle
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 给一个 UTF-8 字符串里的字节索引，返回下一个字符的字节边界。
/// 保证不切到多字节序列中间。
fn next_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut j = i + 1;
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

/// 从 `/foo/bar` 取首段 `foo`，从 `/login` 取 `login`，从 `/` 或 `""` 取 `""`。
/// workspace_id 的获取逻辑只关心当前 URL 的工作空间 slug（单段路由
/// `/:slug`），不需要完整路径解析。
fn first_path_segment(pathname: &str) -> String {
    let trimmed = pathname.trim_start_matches('/');
    trimmed
        .split('/')
        .next()
        .unwrap_or("")
        .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .to_string()
}

/// 会话列表里显示的标题：有 `title` 就用，否则用 `updatedAt` 渲染成
/// `M-D HH:mm`。新建会话 `title` 是空串，标题列没东西渲染用户就
/// 看不见条目；用更新时间占位还能区分同日多次新建。`updatedAt` 是
/// `chrono::DateTime<Utc>` 序列化的 ISO 字符串，手工解前缀避免拖进
/// chrono 这个 wasm-only 重型依赖。
fn session_label(s: &crate::frontend::graphql_client::AgentSession) -> String {
    if !s.title.is_empty() {
        return s.title.clone();
    }
    let iso = &s.updated_at;
    let formatted = (|| -> Option<String> {
        let bytes = iso.as_bytes();
        let take = |start: usize, len: usize| -> Option<&str> {
            let end = start.checked_add(len)?;
            std::str::from_utf8(bytes.get(start..end)?).ok()
        };
        let month = take(5, 2)?;
        let day = take(8, 2)?;
        let hh = take(11, 2)?;
        let mm = take(14, 2)?;
        Some(format!("{month}-{day} {hh}:{mm}"))
    })();
    formatted.unwrap_or_else(|| iso.to_string())
}

/// 视口尺寸（宽, 高），拿不到（SSR / 权限拒绝）返回 None。AgentPanel
/// 用它把新开的浮窗钉到右下角：避免在窄屏上撑出滚动条。
#[cfg(target_arch = "wasm32")]
fn viewport_size() -> Option<(f64, f64)> {
    let win = web_sys::window()?;
    let w = win.inner_width().ok()?.as_f64()?;
    let h = win.inner_height().ok()?.as_f64()?;
    Some((w, h))
}

#[cfg(not(target_arch = "wasm32"))]
fn viewport_size() -> Option<(f64, f64)> {
    None
}

/// 从 localStorage 读出之前落的面板位置 / 尺寸。失败 / 字段缺失一律返回 None，
/// 让调用方走默认定位（视口右下角）。这里不解析错误：localStorage 在隐私模式
/// 下会抛 `SecurityError`，拿 None 比 panic 安全。
#[cfg(target_arch = "wasm32")]
fn load_panel_layout() -> Option<(f64, f64, f64, f64)> {
    let win = web_sys::window()?;
    let storage = win.local_storage().ok()??;
    let raw = storage.get_item("rodeo_agent_panel").ok()??;
    let parts: Vec<&str> = raw.split(',').collect();
    if parts.len() != 4 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
        parts[3].parse().ok()?,
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn load_panel_layout() -> Option<(f64, f64, f64, f64)> {
    None
}

/// 把面板当前位置 / 尺寸写回 localStorage，键 `rodeo_agent_panel`。
/// 失败只 warn 不抛——面板能继续拖，只是下次刷新会回到默认位置。
#[cfg(target_arch = "wasm32")]
fn save_panel_layout(left: f64, top: f64, width: f64, height: f64) {
    if let Some(win) = web_sys::window() {
        if let Ok(Some(storage)) = win.local_storage() {
            let s = format!("{left},{top},{width},{height}");
            if let Err(e) = storage.set_item("rodeo_agent_panel", &s) {
                web_sys::console::warn_1(&format!("save_panel_layout 失败: {e:?}").into());
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn save_panel_layout(_left: f64, _top: f64, _width: f64, _height: f64) {}

#[cfg(target_arch = "wasm32")]
fn web_sys_console_warn(msg: &str) {
    web_sys::console::warn_1(&msg.into());
}

#[cfg(not(target_arch = "wasm32"))]
fn web_sys_console_warn(_msg: &str) {}

/// 把 `workspace_by_slug` 抛出来的 GraphQL / 网络错误归到人可读类别。
/// 匹配的是 `error.rs` 里 `AppError` 的 Display 字面量（"未授权" /
/// "无权限执行此操作"），因为 async-graphql 7 的 blanket `From<T: Display>`
/// 不走 `ErrorExtensions::extend`，extensions.code 实际拿不到——和
/// `service/agent/exec.rs::classify_tool_error` 同款困境。这里只用来打
/// 分类日志，不是路由决策，匹配不到就归「网络或未知」。
fn classify_workspace_error(msg: &str) -> &'static str {
    if msg.contains("未授权") {
        "未授权"
    } else if msg.contains("无权限") {
        "无权限"
    } else if msg == "GraphQL 错误" || msg.is_empty() {
        "GraphQL 协议错误"
    } else {
        "网络或未知错误"
    }
}

/// 把 SSE 端点接回来 —— 用 `fetch + ReadableStream` 而非 `EventSource`，
/// 原因是浏览器 `EventSource` 没法设 `Authorization` 头，而当前仓库
/// `/api/agent/turns/<id>/stream` 走 cookie→Bearer 双源鉴权
/// （`extract_auth`）。本任务用 Bearer 头，未来若服务端登录 mutation 改成
/// Set-Cookie，可直接换成 `EventSource` 让 cookie 自动带过去。
///
/// **流式读取**：`ReadableStreamDefaultReader.read()` 拉 chunk（Uint8Array），
/// `TextDecoder('utf-8', {fatal: false})` 解码到字符串 buffer。SSE 事件边界
/// 是 `\n\n`（一个空行），buffer 攒到能切出完整事件就处理一条；遇到不完整
/// 的尾段就留到下次 read。`fatal: false` 让多字节字符被切两半时不抛错（U+FFFD
/// 替换），不会因为 chunk 边界卡死整个流。
#[cfg(target_arch = "wasm32")]
async fn open_stream(
    turn_id: &str,
    messages: RwSignal<Vec<AgentMessage>>,
    streaming: RwSignal<bool>,
    streaming_tool_calls: RwSignal<Vec<ToolCallDisplay>>,
    effects: AgentSideEffects,
) {
    use gloo_net::http::Request;
    use js_sys::{Reflect, Uint8Array};
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{ReadableStreamDefaultReader, TextDecoder};

    let url = format!("/api/agent/turns/{turn_id}/stream");
    let mut req = Request::get(&url).header("Accept", "text/event-stream");
    if let Some(token) = get_token() {
        req = req.header("Authorization", &format!("Bearer {token}"));
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            web_sys::console::warn_1(&format!("SSE fetch failed: {e}").into());
            streaming.set(false);
            return;
        }
    };
    if !resp.ok() {
        web_sys::console::warn_1(&format!("SSE status {}", resp.status()).into());
        streaming.set(false);
        return;
    }
    let stream = match resp.body() {
        Some(s) => s,
        None => {
            web_sys::console::warn_1(&"SSE body is null".into());
            streaming.set(false);
            return;
        }
    };
    let reader: ReadableStreamDefaultReader = match stream.get_reader().dyn_into() {
        Ok(r) => r,
        Err(_) => {
            web_sys::console::warn_1(&"SSE reader dyn_into failed".into());
            streaming.set(false);
            return;
        }
    };
    // TextDecoder 默认 fatal=false：被切两半的多字节字符变 U+FFFD 而不抛错，
    // chunk 边界不会卡死整个流。
    let decoder = TextDecoder::new().unwrap();

    let mut buffer = String::new();
    loop {
        let chunk_js = match JsFuture::from(reader.read()).await {
            Ok(v) => v,
            Err(e) => {
                web_sys::console::warn_1(&format!("SSE read failed: {e:?}").into());
                break;
            }
        };
        // chunk 是 { value: Uint8Array | undefined, done: bool }。done=true 时
        // value 通常是 undefined，直接退出循环。
        let done = Reflect::get(&chunk_js, &JsValue::from_str("done"))
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if done {
            break;
        }
        let bytes: Uint8Array = match Reflect::get(&chunk_js, &JsValue::from_str("value"))
            .ok()
            .and_then(|v| v.dyn_into::<Uint8Array>().ok())
        {
            Some(b) => b,
            None => continue,
        };
        // Uint8Array 没有 Deref<Target=[u8]>，要先 to_vec 再借片给 decode。
        let vec = bytes.to_vec();
        let text = match decoder.decode_with_u8_array(&vec) {
            Ok(s) => s,
            Err(_) => continue,
        };
        buffer.push_str(&text);
        // 切出所有完整事件（一个 `\n\n` 收尾）；剩余的留到下次 read。
        while let Some(idx) = buffer.find("\n\n") {
            let raw_event: String = buffer.drain(..idx + 2).collect();
            if let Some(event_payload) = parse_sse_event(&raw_event) {
                let parsed: Value = match serde_json::from_str(&event_payload) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                handle_event(&parsed, &messages, &streaming_tool_calls, &effects);
            }
        }
    }
    streaming.set(false);
}

/// 从一段 SSE 事件原文（已包含到 `\n\n` 边界）中挑出 `data:` 行拼成的 payload。
/// `data:` 可跨多行（用 `\n` 串起来），但本仓库后端每事件只发一行 data，所以
/// 多行分支其实走不到；留 `push('\n')` 兜底以防未来扩展。
#[cfg(target_arch = "wasm32")]
fn parse_sse_event(raw: &str) -> Option<String> {
    let mut out = String::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(rest.trim_start());
        }
    }
    if out.is_empty() || out == "end" {
        return None;
    }
    Some(out)
}

#[cfg(not(target_arch = "wasm32"))]
async fn open_stream(
    _turn_id: &str,
    _messages: RwSignal<Vec<AgentMessage>>,
    streaming: RwSignal<bool>,
    _streaming_tool_calls: RwSignal<Vec<ToolCallDisplay>>,
    _effects: AgentSideEffects,
) {
    streaming.set(false);
}

#[cfg(target_arch = "wasm32")]
fn handle_event(
    parsed: &Value,
    messages: &RwSignal<Vec<AgentMessage>>,
    streaming_tool_calls: &RwSignal<Vec<ToolCallDisplay>>,
    effects: &AgentSideEffects,
) {
    let typ = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match typ {
        "delta" => {
            let content = parsed
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            messages.update(|m| {
                if let Some(last) = m.last_mut() {
                    if last.role == "ASSISTANT" {
                        last.content.push_str(&content);
                        return;
                    }
                }
                m.push(AgentMessage {
                    id: ulid_compat_id(),
                    session_id: String::new(),
                    role: "ASSISTANT".to_string(),
                    content,
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    created_at: String::new(),
                });
            });
        }
        "reasoning" => {
            // 推理链与 Delta 各自累积——同一回合里 assistant 消息只有一条,
            // reasoning / content 分别写在两个字段上,渲染时分两块。
            let content = parsed
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            messages.update(|m| {
                if let Some(last) = m.last_mut() {
                    if last.role == "ASSISTANT" {
                        last.reasoning.push_str(&content);
                        return;
                    }
                }
                // 罕见：推理先于第一条 Delta 到达——先建一个空 content 的占位。
                m.push(AgentMessage {
                    id: ulid_compat_id(),
                    session_id: String::new(),
                    role: "ASSISTANT".to_string(),
                    content: String::new(),
                    reasoning: content,
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    created_at: String::new(),
                });
            });
        }
        "tool_call" => {
            // 流式 ToolCall 卡片：push 新元素，状态 Running。AgentEvent::ToolCall
            // 没有 id 字段（rig 0.42 不带 tool_call_id），只能按 name 配对——
            // 用 ulid_compat_id 给前端一个稳定 key。
            let name = parsed
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args = parsed
                .get("args")
                .cloned()
                .unwrap_or(Value::Null);
            streaming_tool_calls.update(|tcs| {
                tcs.push(ToolCallDisplay {
                    id: ulid_compat_id(),
                    name,
                    args,
                    status: ToolCallStatus::Running,
                    preview: String::new(),
                });
            });
        }
        "tool_result" => {
            // 配对：找到同名 + 仍在 Running 的最早元素（FIFO），写入 status + preview。
            // 同名并发 tool_call 的极端情况下 FIFO 会把后到的结果挂到前一个调用
            // 上——但 rig 当前是顺序执行工具,实际碰不到,留 FIFO 是因为它对常见
            // 场景（多个不同名字的工具串行）天然正确。
            // 找不到匹配时（孤儿 ToolResult）也 push 一张卡，保证用户至少看见结果。
            let name = parsed
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let preview = parsed
                .get("preview")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let kind = parsed
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("ok");
            let ok = matches!(kind, "ok");
            let target_status = if ok { ToolCallStatus::Ok } else { ToolCallStatus::Err };
            // 先用 snapshot 找一遍索引,再用 update 改——RwSignal::update 签名
            // 返回 ()，把匹配结果塞在 update 闭包里反而拿不出来。
            let idx = streaming_tool_calls
                .get_untracked()
                .iter()
                .position(|tc| tc.name == name && tc.status == ToolCallStatus::Running);
            if let Some(i) = idx {
                streaming_tool_calls.update(|tcs| {
                    if let Some(tc) = tcs.get_mut(i) {
                        tc.status = target_status;
                        tc.preview = preview.clone();
                    }
                });
            } else {
                streaming_tool_calls.update(|tcs| {
                    tcs.push(ToolCallDisplay {
                        id: ulid_compat_id(),
                        name,
                        args: Value::Null,
                        status: target_status,
                        preview,
                    });
                });
            }
        }
        "side_effect" => {
            let se = parsed.get("side_effect").cloned().unwrap_or(Value::Null);
            let hint = SideEffectHint {
                domain: se
                    .get("domain")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                action: se
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                id: se
                    .get("ref_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| se.get("id").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_string(),
                payload: se
                    .get("payload")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            };
            effects.tick.update(|n| *n += 1);
            effects.last.set(Some(hint));
        }
        "done" | "error" => {
            // 流结束由调用方在 open_stream 末尾统一把 streaming 置回 false，
            // 这里不再重复处理。但要把 streaming_tool_calls 清掉：list_agent_messages
            // 重新拉到真消息后,真消息自带 m.tool_calls（结构化、完整配对）,
            // 卡片渲染会从那个字段来——这里的乐观态不撤掉会和真消息重复。
            streaming_tool_calls.set(Vec::new());
        }
        _ => {}
    }
}

/// 与服务端 `derive_session_title` 同算法：trim 后取首段非空 line，截断
/// 30 字 + `…`。本地 on_send 成功后乐观更新 sessions 缓存，避免再发一次
/// list_agent_sessions。改任一处算法时记得同步另一边。
fn local_session_title(content: &str) -> Option<String> {
    const MAX_CHARS: usize = 30;
    let first_line = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())?;
    let truncated: String = first_line.chars().take(MAX_CHARS).collect();
    if first_line.chars().count() > MAX_CHARS {
        Some(format!("{truncated}…"))
    } else {
        Some(truncated)
    }
}

/// 把 `change` 事件里选中的文件逐个上传到 agent 会话附件后端。
/// 与 `attachment_list::spawn_upload` 同思路：通过 `Reflect::get("files")` 拿
/// FileList（仓库没开 web-sys 的 `FileList` 特性）；逐个并发上传；上传
/// 完成后把 `(id, filename, size)` 推进 `attachments` 信号；任一文件失败
/// 把错误挂到 `attachments_error`，不影响已成功的。
#[cfg(target_arch = "wasm32")]
fn spawn_attachment_uploads(
    ev: leptos::ev::Event,
    session_id: Option<String>,
    attachments: RwSignal<Vec<AgentAttachmentChip>>,
    uploading: RwSignal<bool>,
    error: RwSignal<Option<String>>,
) {
    use js_sys::Reflect;
    use wasm_bindgen::{JsCast, JsValue};

    let Some(sid) = session_id else {
        error.set(Some("请先选择会话再添加附件".to_string()));
        return;
    };
    let Some(input) = ev
        .target()
        .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
        return;
    };
    let Ok(files_js) =
        Reflect::get(AsRef::<JsValue>::as_ref(&input), &JsValue::from_str("files"))
    else {
        return;
    };
    // FileList 没有数值属性（Reflect::get(files, idx) 拿到 undefined），也不能
    // dyn_ref::<js_sys::Array>（FileList 不是 Array）。最稳的路是
    // `Array.from(files)`——FileList 自带 Symbol.iterator，转出来的就是普通
    // js_sys::Array，每个元素是 File。`attachment_list::spawn_upload` 只读
    // 0 号走的是巧合：浏览器实现里 Reflect::get(files, 0) 偶尔返回 File，
    // 但写 `for i in 0..n` 循环就会拿到 undefined 静默失败。
    // 注：`Array(fileList)` 单参调用只会按 length 构 `[undefined, ...]`，
    // 不是 `Array.from`，所以必须显式取 `Array.from`。
    let Ok(array_ctor) = js_sys::eval("Array") else {
        uploading.set(false);
        return;
    };
    let array_ctor: js_sys::Function = array_ctor.unchecked_into();
    let array_from = match Reflect::get(array_ctor.as_ref(), &JsValue::from_str("from"))
        .ok()
        .and_then(|v| v.dyn_into::<js_sys::Function>().ok())
    {
        Some(f) => f,
        None => {
            uploading.set(false);
            return;
        }
    };
    let Ok(files_arr_js) = array_from.call1(&JsValue::UNDEFINED, &files_js) else {
        uploading.set(false);
        return;
    };
    let Some(files_arr) = files_arr_js.dyn_ref::<js_sys::Array>() else {
        uploading.set(false);
        return;
    };
    let length_js = files_arr.length();
    if length_js == 0 {
        return;
    }
    // 选完即清 input.value，下次同文件也能再触发 change。
    input.set_value("");
    uploading.set(true);
    error.set(None);

    // 每个文件独立 spawn 一个 task，并发上传；哪个先完成先 update 信号。
    // 不引入 futures::join_all：仓库只挂 futures-util，开新 crate 不值得。
    // 用 `AtomicI64` 计数——最后一个任务负责把 uploading 翻回 false。
    // 计数本身包 Arc：spawn_local(async move) 会 move 捕获，循环里同一变量要 clone 进每个任务。
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;
    let remaining = Arc::new(AtomicI64::new(length_js as i64));

    for i in 0..length_js {
        let file_js = match files_arr.get(i as u32) {
            v if !v.is_undefined() && !v.is_null() => v,
            _ => {
                if remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
                    uploading.set(false);
                }
                continue;
            }
        };
        let size = Reflect::get(&file_js, &JsValue::from_str("size"))
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as i64;
        let filename = Reflect::get(&file_js, &JsValue::from_str("name"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "未命名文件".to_string());
        // Array.from 出来的元素仍是 File 实例，但 wasm-bindgen 的
        // dyn_into::<web_sys::File> 走静态 prototype 检查会失败（FileList
        // / 跨 realm 场景常见），改走 unchecked_from_js：File 继承 Blob，
        // 运行期 instanceof 也通过，uploadAttachment 内部只用 Blob 字段。
        let file_for_upload = web_sys::File::unchecked_from_js(file_js);
        let sid_for_task = sid.clone();
        let remaining_for_task = remaining.clone();
        spawn_local(async move {
            let result = upload_attachment("", &file_for_upload, false, Some(&sid_for_task)).await;
            match result {
                Ok(a) => {
                    attachments.update(|l| {
                        l.push(AgentAttachmentChip {
                            id: a.id,
                            filename,
                            size,
                        });
                    });
                }
                Err(e) => {
                    let new_msg = format!("{filename} 上传失败: {e}");
                    error.update(|prev| {
                        *prev = match prev.take() {
                            Some(prev_msg) => Some(format!("{prev_msg}; {new_msg}")),
                            None => Some(new_msg),
                        };
                    });
                }
            }
            if remaining_for_task.fetch_sub(1, Ordering::SeqCst) == 1 {
                uploading.set(false);
            }
        });
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_attachment_uploads(
    _ev: leptos::ev::Event,
    _session_id: Option<String>,
    _attachments: RwSignal<Vec<AgentAttachmentChip>>,
    _uploading: RwSignal<bool>,
    _error: RwSignal<Option<String>>,
) {
}
