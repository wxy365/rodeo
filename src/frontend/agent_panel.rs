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
use crate::frontend::graphql_client::{
    create_agent_session, delete_agent_session, list_agent_messages, list_agent_sessions,
    send_agent_message, AgentMessage, AgentSession,
};

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

#[component]
pub fn AgentPanel() -> impl IntoView {
    let open = use_agent_panel_open();
    let effects = use_context::<AgentSideEffects>().expect("AgentSideEffects 未提供");

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
    let streaming: RwSignal<bool> = RwSignal::new(false);

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
    });

    let on_send = move |_| {
        // 闭包不在 setup 段；`.get_untracked()` 拿调用时刻的快照即可——
        // send 后 streaming / input 的变化不需要回头更新这个回调。
        let content = input.get_untracked();
        if content.trim().is_empty() {
            return;
        }
        let Some(sid) = current_session.get_untracked() else {
            web_sys_console_warn("no session selected");
            return;
        };
        let Some(ws_id) = workspace_id_resolved.get_untracked() else {
            web_sys_console_warn("workspace not ready");
            return;
        };
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
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: String::new(),
            });
        });
        input.set(String::new());
        streaming.set(true);
        let sid_for_refresh = sid.clone();
        spawn_local(async move {
            match send_agent_message(&sid, content).await {
                Ok(turn) => {
                    // 占位自然由 open_stream 之后的 post-stream list_agent_messages
                    // 替换为真 ULID 消息——SSE 路径出错也不需要就地清，因为
                    // 服务端已经把 user_msg 落库（graphql.rs send_agent_message
                    // 先 append 再 run_turn），list_agent_messages 能拿回真消息。
                    open_stream(&turn.id, messages, streaming, effects).await;
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
                                                    <span class="agent-session-title">{s.title}</span>
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
                                    <div class="agent-messages">
                                        <For
                                            each=move || messages.get()
                                            key=|m| m.id.clone()
                                            children=|m: AgentMessage| {
                                                let html = render_markdown(&m.content);
                                                view! {
                                                    <div class=format!("agent-msg agent-msg-{}", m.role.to_lowercase()) inner_html=html></div>
                                                }
                                            }
                                        />
                                    </div>
                                    <div class="agent-input">
                                        <textarea
                                            class="agent-textarea"
                                            bind:value=input
                                            prop:disabled=move || streaming.get()
                                            placeholder="向 Buckaroo 提问…"
                                        />
                                        <button
                                            class="agent-send"
                                            on:click=on_send
                                            prop:disabled=move || streaming.get()
                                        >
                                            {move || if streaming.get() { "..." } else { "发送" }}
                                        </button>
                                    </div>
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
fn ulid_compat_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("local-{t}")
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
    let reader = match stream
        .dyn_into::<ReadableStreamDefaultReader>()
        .map_err(|_| ())
    {
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
                handle_event(&parsed, &messages, &effects);
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
    _effects: AgentSideEffects,
) {
    streaming.set(false);
}

#[cfg(target_arch = "wasm32")]
fn handle_event(parsed: &Value, messages: &RwSignal<Vec<AgentMessage>>, effects: &AgentSideEffects) {
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
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    created_at: String::new(),
                });
            });
        }
        "tool_call" => {
            let name = parsed
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            messages.update(|m| {
                m.push(AgentMessage {
                    id: ulid_compat_id(),
                    session_id: String::new(),
                    role: "TOOL_CALL".to_string(),
                    content: format!("[tool_call] {name}"),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    created_at: String::new(),
                });
            });
        }
        "tool_result" => {
            let preview = parsed
                .get("preview")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let kind = parsed
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("ok");
            let (prefix, role) = match kind {
                "bad_args" => ("[参数错]", "TOOL_RESULT_BAD_ARGS"),
                "rejected" => ("[拒绝]", "TOOL_RESULT_REJECTED"),
                "server_error" => ("[服务端错误]", "TOOL_RESULT_SERVER_ERROR"),
                _ => ("[ok]", "TOOL_RESULT"),
            };
            messages.update(|m| {
                m.push(AgentMessage {
                    id: ulid_compat_id(),
                    session_id: String::new(),
                    role: role.to_string(),
                    content: format!("{prefix} {preview}"),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    created_at: String::new(),
                });
            });
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
            };
            effects.tick.update(|n| *n += 1);
            effects.last.set(Some(hint));
        }
        "done" | "error" => {
            // 流结束由调用方在 open_stream 末尾统一把 streaming 置回 false，
            // 这里不再重复处理。
        }
        _ => {}
    }
}
