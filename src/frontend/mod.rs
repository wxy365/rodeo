pub mod ai_prompt_editor;
pub mod attachment_list;
pub mod automation_tab;
pub mod comment_list;
pub mod components;
pub mod graphql_client;
pub mod icons;
pub mod label_editor;
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
