//! AI Agent 侧拉面板：会话列表 + 消息流 + 输入框。
//!
//! **本文件目前只放最小骨架**：定义 `AgentPanelOpen` 上下文（决定面板开 / 合）
//! 与一个不渲染任何东西的占位组件。完整的事件流订阅、消息累积、SSE 解析、
//! 侧副作用派发留到后续 task（12 / 13）再补——拆开提交是为了让每一步都先
//! 通过 `make check`，免得一次性塞完导致 wasm 端编译失败却没有可回滚的小步。
//!
//! 全局副作用入口见 `crate::frontend::agent_side_effects::AgentSideEffects`。
//! 通信通道见 `crate::frontend::graphql_client::{list_agent_sessions,
//! list_agent_messages, create_agent_session, delete_agent_session,
//! send_agent_message}`（Task 10 接入）。

use leptos::prelude::*;

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
    view! {
        <div class="agent-panel-placeholder" hidden>
            "agent panel"
        </div>
    }
}
