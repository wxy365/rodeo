//! AI Agent 侧拉面板：会话列表 + 消息流 + 输入框。
//!
//! 用例见 spec §14；本任务是骨架的「能跑通一条简单对话」最小可用 UI，
//! 真正的 UX 收尾（Markdown 渲染 / 自动滚动 / tool_call 折叠 / loading
//! spinner / 多会话切换 / 删除会话）留 follow-up。
//!
//! 数据来源：
//! - GraphQL：`list_agent_sessions` / `list_agent_messages` /
//!   `create_agent_session` / `delete_agent_session` / `send_agent_message`
//!   （`crate::frontend::graphql_client`，Task 10 接入）
//! - SSE：`/api/agent/turns/<turn_id>/stream`（Task 7/8），用 `fetch +
//!   ReadableStream` 自己解析（详见 `open_stream` 注释）—— 浏览器
//!   `EventSource` 不能设 `Authorization` 头，必须走 `fetch` 自己补。
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
use leptos_router::hooks::use_params_map;

#[cfg(target_arch = "wasm32")]
use serde_json::Value;

use crate::frontend::agent_side_effects::AgentSideEffects;
#[cfg(target_arch = "wasm32")]
use crate::frontend::agent_side_effects::SideEffectHint;
#[cfg(target_arch = "wasm32")]
use crate::frontend::graphql_client::get_token;
use crate::frontend::graphql_client::{
    create_agent_session, list_agent_messages, list_agent_sessions, send_agent_message, AgentMessage,
    AgentSession,
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

    let params = use_params_map();
    // workspace_by_slug 拿到的 workspace_id：所有会话/消息请求都依赖它。
    // 用 Resource 而不是直接在 setup 段 block_on：AgentPanel 挂在 Router
    // 之外，但 use_params_map 在路由变化时会更新触发 Resource 重跑，
    // —— 用户从 `/foo` 跳到 `/bar` 时面板不需要重建，Resource 自己重新解。
    // 把 `workspace_id` 直接放进 RwSignal —— Resource 在 native target 下要求
    // future 实现 `Send`，而 `gloo_net::http::Request` 内部是 `Rc<RefCell>`，
    // 即使 cfg-gated 也会让 trait solver 把整条 future 推到 `!Send`。改用
    // Effect + spawn_local 直接拉一次，与 SSR 路径上的 workspace_by_slug
    // 行为一致：native 端 graphql() 返回 Err → 这个 Effect 在 SSR 期间无
    // 网络也没副作用，到 hydrate 后浏览器真实运行时会拿到 workspace_id。
    let workspace_id_resolved: RwSignal<Option<String>> = RwSignal::new(None);
    {
        let params = params.clone();
        Effect::new(move |_| {
            let slug = params.get().get("slug").unwrap_or_default();
            if slug.is_empty() {
                workspace_id_resolved.set(None);
                return;
            }
            spawn_local(async move {
                let res = crate::frontend::graphql_client::workspace_by_slug(&slug).await;
                let next = match res {
                    Ok(Some(ws)) => Some(ws.id),
                    _ => None,
                };
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

    // 加载会话列表：open 打开 + workspace_id 就绪时拉一次；切换 workspace 时
    // 也跟着重拉（Resource 自动追踪 key 变化）。
    Effect::new(move |_| {
        let is_open = open.0.get();
        let ws_id = workspace_id_resolved.get();
        if !is_open {
            return;
        }
        let Some(ws_id) = ws_id else { return };
        spawn_local(async move {
            match list_agent_sessions(&ws_id).await {
                Ok(list) => sessions.set(list),
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
        // 乐观写入用户消息（id 由前端生成；后端落库后真正的 id 在
        // list_agent_messages 重拉时回来；当前简化 UI 不去 reconcile）。
        messages.update(|m| {
            m.push(AgentMessage {
                id: ulid_compat_id(),
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
        spawn_local(async move {
            match send_agent_message(&sid, content).await {
                Ok(turn) => {
                    open_stream(&turn.id, messages, streaming, effects).await;
                }
                Err(e) => {
                    streaming.set(false);
                    web_sys_console_warn(&format!("send_agent_message failed: {e}"));
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
            <div class="agent-panel">
                <header class="agent-header">
                    <span class="agent-title">"AI Agent"</span>
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
                                            view! {
                                                <div
                                                    class="agent-session-item"
                                                    class:agent-session-active=move || current_session.get().as_deref() == Some(sid_for_active.as_str())
                                                    on:click={
                                                        let sid = sid_for_click;
                                                        move |_| current_session.set(Some(sid.clone()))
                                                    }
                                                >
                                                    <span>{s.title}</span>
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
                                            children=|m: AgentMessage| view! {
                                                <div class=format!("agent-msg agent-msg-{}", m.role.to_lowercase())>
                                                    <pre>{m.content.clone()}</pre>
                                                </div>
                                            }
                                        />
                                    </div>
                                    <div class="agent-input">
                                        <textarea
                                            class="agent-textarea"
                                            bind:value=input
                                            prop:disabled=move || streaming.get()
                                            placeholder="向 AI Agent 提问…"
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

#[cfg(target_arch = "wasm32")]
fn web_sys_console_warn(msg: &str) {
    web_sys::console::warn_1(&msg.into());
}

#[cfg(not(target_arch = "wasm32"))]
fn web_sys_console_warn(_msg: &str) {}

/// 把 SSE 端点接回来 —— 用 `fetch + ReadableStream` 而非 `EventSource`，
/// 原因是浏览器 `EventSource` 没法设 `Authorization` 头，而当前仓库
/// `/api/agent/turns/<id>/stream` 走 cookie→Bearer 双源鉴权
/// （`extract_auth`）。本任务用 Bearer 头，未来若服务端登录 mutation 改成
/// Set-Cookie，可直接换成 `EventSource` 让 cookie 自动带过去。
///
/// **简化边界**：本实现一次性读完整个 body 再按 `\n\n` 切事件，**牺牲
/// 流式实时性换实现简单**。真实 UX 需要增量读取（ReadableStreamDefaultReader
/// + TextDecoder）作为 follow-up。Smoke 用例只验证「能拿到首条 delta」
/// 在 done/error 帧到达时正常结束。
#[cfg(target_arch = "wasm32")]
async fn open_stream(
    turn_id: &str,
    messages: RwSignal<Vec<AgentMessage>>,
    streaming: RwSignal<bool>,
    effects: AgentSideEffects,
) {
    use gloo_net::http::Request;

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
    let body = match resp.text().await {
        Ok(s) => s,
        Err(e) => {
            web_sys::console::warn_1(&format!("SSE body read failed: {e}").into());
            streaming.set(false);
            return;
        }
    };
    for raw_event in body.split("\n\n") {
        if raw_event.is_empty() {
            continue;
        }
        // SSE 协议：每行 `field: value`；这里只关心 `data:` 行（后端只发
        // 默认事件类型 + `event: close`，前者在 `Event::default().data(...)`）。
        let mut data_payload = String::new();
        for line in raw_event.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                if !data_payload.is_empty() {
                    data_payload.push('\n');
                }
                data_payload.push_str(rest.trim_start());
            }
        }
        if data_payload.is_empty() || data_payload == "end" {
            // `event: close` 帧 = 服务端 Done/Error 后的退出信号，吞掉即可。
            continue;
        }
        let parsed: Value = match serde_json::from_str(&data_payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        handle_event(&parsed, &messages, &effects);
    }
    streaming.set(false);
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
            messages.update(|m| {
                m.push(AgentMessage {
                    id: ulid_compat_id(),
                    session_id: String::new(),
                    role: "TOOL_RESULT".to_string(),
                    content: format!("[result] {preview}"),
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
