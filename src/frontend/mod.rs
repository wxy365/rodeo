pub mod graphql_client;
pub mod pages;

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
