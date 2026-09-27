//! 全局 Agent 副作用 store：
//! - `tick`：自增计数，订阅端 `.get()` 然后 `effect(move || ...)`，
//!   每次变化触发刷新（`last` 也参与判等避免漏刷新）。
//! - `last`：最近一次 SideEffectHint；订阅端用它判断要不要重查。
//!
//! 提供位置由调用方决定（应在 App 层、`<Router>` 之外，与 AuthState 同层）。

use leptos::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy)]
pub struct AgentSideEffects {
    pub tick: RwSignal<u64>,
    pub last: RwSignal<Option<SideEffectHint>>,
}

/// 前端可见的最小副作用描述：领域 + 动作 + 资源 ID。
/// 服务端 `SideEffect` 枚举转成的纯数据形态——具体形状由 AgentPanel
///（Task 11）反序列化时按 GraphQL 字段填充。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SideEffectHint {
    pub domain: String,
    pub action: String,
    pub id: String,
}

pub fn provide_agent_side_effects() -> AgentSideEffects {
    let effects = AgentSideEffects {
        tick: RwSignal::new(0u64),
        last: RwSignal::new(None),
    };
    provide_context(effects);
    effects
}

pub fn use_agent_side_effects() -> AgentSideEffects {
    use_context::<AgentSideEffects>().expect("AgentSideEffects 未提供")
}