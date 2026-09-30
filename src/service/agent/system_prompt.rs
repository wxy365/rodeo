//! Agent 用的 system prompt 与历史消息组装。
//!
//! 这两个 helper 从 `src/service/ai.rs` 拆出来：ai.rs 现在只关心 Rig 装配和配置层
//! 逻辑，runner（task 10 改写）会从这里拿 system 文本和历史 `AgentMessage` 序列，
//! 喂给 Rig 的 Chat Completions streaming。
//!
//! 注意：`to_chat_messages` 现在还是「把 `&[AgentMessage]` 原样转 `Vec<AgentMessage>`」
//! 的恒等形态，因为 `AgentMessage` 仍是领域层类型，task 3 会把它换成
//! `rig::message::Message` 别名，task 11 会按新类型重写 `to_chat_messages`。
//! 提前按 rig 的 `Message` 写只会让 task 3 一次性大改两次。

use crate::domain::AgentMessage;

/// 通用 system 提示 + 工作空间附加段。返回纯文本，runner 自己把它塞进首条
/// system 消息。
pub fn agent_system_prompt(ws_title: &str, ws_id: &str) -> String {
    format!(
        "你是 Rodeo 工作空间内的对话式 Agent。你可以调用工具查询和修改条目、标签、评论、视图等数据。\
         所有工具调用都在当前用户权限下执行——管理员才能改的，你查不到也改不了。\
         执行多条工具调用时，按依赖顺序串行：先 list 再 update，避免基于过期信息修改。\
         一次工具调用失败就停下来告诉用户，不要反复重试同一参数。\
         回答使用 Markdown，简洁优先。\n\n\
         任务上下文（视图 / 选中条目 / 附件清单）会作为本轮的第二条 user 消息注入，\
         这是当前会话的真实状态数据，请据此作答，但不要回复这条上下文本身。\n\n\
         当前工作空间：{ws_title}\n\
         工作空间 ID：{ws_id}"
    )
}

/// 当前只是把领域层 `AgentMessage` 序列按值复制一份：
/// `AgentMessage` 现在还带着 `id` / `session_id` / `created_at` 这些域字段，序列化
/// 时要全部透传，不做任何过滤或转换。task 3 切到 `rig::message::Message` 之后，本
/// 函数会做 role / tool_calls / tool_call_id 的实际映射。
pub fn to_chat_messages(history: &[AgentMessage]) -> Vec<AgentMessage> {
    history.to_vec()
}
