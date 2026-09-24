pub mod ai_prompt_editor;
pub mod attachment_list;
pub mod automation_tab;
pub mod comment_list;
pub mod components;
pub mod graphql_client;
pub mod icons;
pub mod label_editor;
pub mod message_list;
pub mod pages;
pub mod query_eval;
pub mod timeline;
pub mod tiny_editor;
pub mod view_filter;

use leptos::prelude::*;

pub use graphql_client::User;

#[derive(Clone, Copy)]
pub struct AuthState {
    pub user: RwSignal<Option<User>>,
    /// 本地令牌被服务端拒绝过（过期或被吊销）。
    ///
    /// `user` 为 `None` 兼有「me() 还在路上」与「没有会话」两义，光看它分不出该显示
    /// 「加载中…」还是「请重新登录」；这个位补上缺的那一维。启动期 `me` 拿到 null 时置真，
    /// 同时那张死令牌已被清掉。
    pub session_lost: RwSignal<bool>,
}

pub fn provide_auth() -> AuthState {
    let state = AuthState {
        user: RwSignal::new(None),
        session_lost: RwSignal::new(false),
    };
    provide_context(state);
    state
}

pub fn use_auth() -> AuthState {
    use_context::<AuthState>().expect("AuthState 未提供")
}

/// 工作空间顶栏「新建」按钮的回调。
///
/// 全局 `AppBar` 是登录后任意路由都渲染的，但「新建 Entry / 新建视图」只对
/// WorkspaceMain 有意义——它俩需要这套页面里的 `show_new`、`active_view`、
/// `query_ast`、`schemas` 等信号。WorkspaceMain 挂载时往槽里写值，AppBar
/// 看到就显示按钮、看不到就什么都不渲染：不要在 AppBar 里再读路由做条件
/// 分支，路由名稳定不下来，加一层页面名映射纯属给自己埋坑。
#[derive(Clone, Copy)]
pub struct WorkspaceNewMenu {
    pub on_new_entry: Callback<()>,
    pub on_new_view: Callback<()>,
}

/// 在 App 层提供的一次性槽，AppBar 与 WorkspaceMain 共享同一份读写权。
///
/// 直接 `provide_context(WorkspaceNewMenu)` 行不通：AppBar 渲染在 `<Router>`
/// 之外，是 WorkspaceMain 的兄弟节点；Leptos 上下文只沿组件树向下传播，
/// 兄弟之间互相拿不到。改成 App 层 `provide_context` 一个
/// `RwSignal<Option<...>>`，两边 `use_context` 各取一份指针，就
/// 写得到、读得到了。
#[derive(Clone, Copy)]
pub struct WorkspaceNewMenuSlot {
    pub current: RwSignal<Option<WorkspaceNewMenu>>,
}

pub fn provide_workspace_new_menu_slot() -> WorkspaceNewMenuSlot {
    let slot = WorkspaceNewMenuSlot {
        current: RwSignal::new(None),
    };
    provide_context(slot);
    slot
}

pub fn use_workspace_new_menu() -> Option<WorkspaceNewMenu> {
    use_context::<WorkspaceNewMenuSlot>().and_then(|s| s.current.get())
}

/// 工作空间顶栏时间轴切换按钮的回调与当前态。
///
/// 与 `WorkspaceNewMenu` 同样的兄弟槽问题，所以单开一个槽而不是合并进去——
/// 「当前视图是否配了时间轴」是 WorkspaceMain 内的判断，不属于 AppBar
/// 该背负的状态；让 WorkspaceMain 在确实有 timeline 配置时才写槽，
/// AppBar 看槽是否非空来决定渲染图标。
#[derive(Clone, Copy)]
pub struct WorkspaceTimelineToggle {
    /// 点击后切到另一头（普通视图 ↔ 时间轴视图）。
    pub on_toggle: Callback<()>,
    /// 当前是否处于时间轴模式——驱动 AppBar 图标的视觉态。
    pub is_timeline_mode: RwSignal<bool>,
}

#[derive(Clone, Copy)]
pub struct WorkspaceTimelineSlot {
    pub current: RwSignal<Option<WorkspaceTimelineToggle>>,
}

pub fn provide_workspace_timeline_slot() -> WorkspaceTimelineSlot {
    let slot = WorkspaceTimelineSlot {
        current: RwSignal::new(None),
    };
    provide_context(slot);
    slot
}

pub fn use_workspace_timeline_toggle() -> Option<WorkspaceTimelineToggle> {
    use_context::<WorkspaceTimelineSlot>().and_then(|s| s.current.get())
}
