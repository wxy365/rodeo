pub mod ai_prompt_editor;
pub mod automation_tab;
pub mod comment_list;
pub mod components;
pub mod graphql_client;
pub mod icons;
pub mod label_editor;
pub mod pages;
pub mod query_eval;
pub mod tiny_editor;
pub mod view_filter;

use leptos::prelude::*;

pub use graphql_client::User;

#[derive(Clone, Copy)]
pub struct AuthState {
    pub user: RwSignal<Option<User>>,
}

pub fn provide_auth() -> AuthState {
    let state = AuthState {
        user: RwSignal::new(None),
    };
    provide_context(state);
    state
}

pub fn use_auth() -> AuthState {
    use_context::<AuthState>().expect("AuthState 未提供")
}
