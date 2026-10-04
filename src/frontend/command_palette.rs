//! 全局命令面板（Cmd/Ctrl+K）。
//!
//! 三类结果合并在一个列表里：
//! 1. **静态命令**：跳工作空间列表、跳账户、跳管理员、切换主题等。
//! 始终显示。
//! 2. **工作空间**：当前用户已加入的。点击直接导航。
//! 3. **条目**：当前工作空间内关键字搜索。空字符串时不出条目组。
//!
//! 交互细节：
//! - `Cmd+K` / `Ctrl+K` 开 / 关（同一个键再次按下关闭）。
//! - `Esc` 关。
//! - 上下键在结果间移动；回车选中；点击选中。
//! - 文本框里的输入跟结果过滤实时联动；条目组的搜索请求每次按键都发。
//! - 路由变化时自动关闭（避免「面板开着结果却跳到新页」）。
//!
//! 提供者模式：与 `AgentPanelOpen` 同款——
//! App 层 `provide_command_palette_open()`，组件内 `use_command_palette_open()`。

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_location, use_navigate};

use super::components::short_time;
use super::graphql_client::{search_entries, workspaces, Entry, WorkspaceItem};
use super::i18n::{use_i18n, I18nController};
use super::icons::{ic_folder, ic_history, ic_logout, ic_profile, ic_search, ic_setting, ic_tag};
use super::theme::ThemeController;
use super::use_auth;

/// 命令面板开关。
#[derive(Clone, Copy)]
pub struct CommandPaletteOpen(pub RwSignal<bool>);

pub fn provide_command_palette_open() -> CommandPaletteOpen {
    let s = CommandPaletteOpen(RwSignal::new(false));
    provide_context(s);
    s
}

pub fn use_command_palette_open() -> CommandPaletteOpen {
    use_context::<CommandPaletteOpen>().expect("CommandPaletteOpen 未提供")
}

/// 单条结果。
#[derive(Clone)]
struct PaletteItem {
    kind: ItemKind,
    title: String,
    subtitle: String,
}

#[derive(Clone)]
enum ItemKind {
    Command(PaletteCommand),
    Workspace(String),
    Entry(String, String),
}

#[derive(Clone, Copy)]
enum PaletteCommand {
    GoWorkspaces,
    GoAccount,
    GoAdmin,
    GoSettings,
    ThemeToggle,
}

/// 命令面板本体。挂在 App 层 Router 内、main 旁。
#[component]
pub fn CommandPalette() -> impl IntoView {
    let i18n: I18nController = use_i18n();
    let open = use_command_palette_open();
    let auth = use_auth();
    let navigate = use_navigate();
    let location = use_location();

    // 路由变化时关闭面板。
    {
        let open_sig = open.0;
        Effect::new(move |_| {
            let _ = location.pathname.get();
            open_sig.set(false);
        });
    }

    let query = RwSignal::new(String::new());
    let selected = RwSignal::new(0usize);
    let ws_cache: RwSignal<Vec<WorkspaceItem>> = RwSignal::new(Vec::new());
    let entry_results: RwSignal<Vec<Entry>> = RwSignal::new(Vec::new());

    let current_slug = move || {
        let p = location.pathname.get();
        let s = p.strip_prefix('/').unwrap_or("");
        if s.is_empty()
            || s.starts_with("login")
            || s.starts_with("workspaces")
            || s.starts_with("admin")
            || s.starts_with("account")
            || s.starts_with("api")
        {
            String::new()
        } else {
            s.split('/').next().unwrap_or("").to_string()
        }
    };

    let current_workspace_id = move || {
        let slug = current_slug();
        if slug.is_empty() {
            return String::new();
        }
        ws_cache
            .get()
            .into_iter()
            .find(|w| w.workspace.slug == slug)
            .map(|w| w.workspace.id.clone())
            .unwrap_or_default()
    };

    // 拉工作空间列表缓存。
    {
        let ws_cache = ws_cache;
        Effect::new(move |_| {
            if !cfg!(target_arch = "wasm32") {
                return;
            }
            if auth.user.get().is_none() {
                return;
            }
            if !open.0.get() {
                return;
            }
            spawn_local(async move {
                if let Ok(list) = workspaces().await {
                    ws_cache.set(list);
                }
            });
        });
    }

    // 条目搜索。
    {
        let entry_results = entry_results;
        Effect::new(move |_| {
            if !cfg!(target_arch = "wasm32") {
                return;
            }
            let q = query.get();
            let ws_id = current_workspace_id();
            if q.trim().is_empty() || ws_id.is_empty() {
                entry_results.set(Vec::new());
                return;
            }
            let q_clone = q.clone();
            let ws_id_clone = ws_id.clone();
            let entry_results_inner = entry_results;
            spawn_local(async move {
                if let Ok(page) = search_entries(&ws_id_clone, &q_clone, 10).await {
                    entry_results_inner.set(page.items);
                }
            });
        });
    }

    let filtered = move || {
        let q = query.get().trim().to_lowercase();
        let mut items: Vec<PaletteItem> = Vec::new();

        items.push(PaletteItem {
            kind: ItemKind::Command(PaletteCommand::GoWorkspaces),
            title: i18n.t("cmdpal.cmd_workspaces"),
            subtitle: i18n.t("cmdpal.cmd_workspaces_sub"),
        });
        items.push(PaletteItem {
            kind: ItemKind::Command(PaletteCommand::GoAccount),
            title: i18n.t("cmdpal.cmd_account"),
            subtitle: i18n.t("cmdpal.cmd_account_sub"),
        });
        items.push(PaletteItem {
            kind: ItemKind::Command(PaletteCommand::GoAdmin),
            title: i18n.t("cmdpal.cmd_admin"),
            subtitle: i18n.t("cmdpal.cmd_admin_sub"),
        });
        let slug = current_slug();
        if !slug.is_empty() {
            items.push(PaletteItem {
                kind: ItemKind::Command(PaletteCommand::GoSettings),
                title: i18n.tf("cmdpal.cmd_settings", &[("slug", &slug)]),
                subtitle: i18n.t("cmdpal.cmd_settings_sub"),
            });
        }
        items.push(PaletteItem {
            kind: ItemKind::Command(PaletteCommand::ThemeToggle),
            title: i18n.t("cmdpal.cmd_theme"),
            subtitle: i18n.t("cmdpal.cmd_theme_sub"),
        });

        for w in ws_cache.get() {
            if w.workspace.deleted_at.is_some() {
                continue;
            }
            items.push(PaletteItem {
                kind: ItemKind::Workspace(w.workspace.slug.clone()),
                title: w.workspace.name.clone(),
                subtitle: format!("/{}", w.workspace.slug),
            });
        }

        for e in entry_results.get() {
            items.push(PaletteItem {
                kind: ItemKind::Entry(e.code.clone(), e.code.clone()),
                title: e.title.clone(),
                subtitle: format!("#{} · {}", e.code, short_time(&e.updated_at)),
            });
        }

        if !q.is_empty() {
            items.retain(|it| {
                it.title.to_lowercase().contains(&q)
                    || it.subtitle.to_lowercase().contains(&q)
            });
        }
        items.truncate(40);
        items
    };

    // 选中越界时夹到 0。
    {
        let selected = selected;
        Effect::new(move |_| {
            let len = filtered().len();
            if selected.get() >= len && len > 0 {
                selected.set(0);
            }
        });
    }

    // 关闭面板：单独抽出 `close_palette` 闭包，避免每处 on:click / on:keydown
    // 都重写一遍 open.set(false) + query.set(...) + selected.set(0)。
    let close_palette = move || {
        open.0.set(false);
        query.set(String::new());
        selected.set(0);
    };

    // 触发一项的回调：Callback<PaletteItem> 是 Rc<dyn Fn>，在闭包里
    // 反复 run() 不会消费 captured 的 navigate——这是 view! 闭包要
    // 求 Fn-compatible 的根本出路。
    //
    // `current_slug` 捕获了 `Location`，`Location` 含 `Memo` /
    // `ReadSignal` 都不是 Copy，因此 `current_slug` 也不是 Copy。
    // 这里 clone 一份交给 do_invoke，闭包本身仍为 Fn。
    let invoke_cb: Callback<PaletteItem> = Callback::new(move |item: PaletteItem| {
        do_invoke(
            item,
            navigate.clone(),
            open.0,
            query,
            selected,
            current_slug.clone(),
        );
    });

    view! {
        <Show when=move || open.0.get()>
            <div class="cmdpal-backdrop" on:click=move |_| close_palette()>
                <div
                    class="cmdpal"
                    on:click=|ev: leptos::ev::MouseEvent| ev.stop_propagation()>
                    <div class="cmdpal-input-row">
                        {ic_search()}
                        <input
                            class="cmdpal-input"
                            placeholder=move || i18n.t("cmdpal.placeholder")
                            prop:value=query
                            on:input=move |ev| {
                                query.set(event_target_value(&ev));
                                selected.set(0);
                            }
                            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                                let len = filtered().len();
                                match ev.key().as_str() {
                                    "ArrowDown" => {
                                        ev.prevent_default();
                                        if len > 0 {
                                            selected.update(|n| *n = (*n + 1) % len);
                                        }
                                    }
                                    "ArrowUp" => {
                                        ev.prevent_default();
                                        if len > 0 {
                                            selected.update(|n| {
                                                if *n == 0 {
                                                    *n = len - 1;
                                                } else {
                                                    *n -= 1;
                                                }
                                            });
                                        }
                                    }
                                    "Enter" => {
                                        ev.prevent_default();
                                        let items = filtered();
                                        if let Some(it) = items.get(selected.get_untracked()) {
                                            invoke_cb.run(it.clone());
                                        }
                                    }
                                    "Escape" => {
                                        ev.prevent_default();
                                        close_palette();
                                    }
                                    _ => {}
                                }
                            }
                        />
                        <span class="cmdpal-kbd-hint">
                            <kbd>"Esc"</kbd>{move || i18n.t("cmdpal.close_hint")}
                        </span>
                    </div>
                    <div class="cmdpal-list">
                        {move || {
                            let items = filtered();
                            if items.is_empty() {
                                return view! { <div class="cmdpal-empty">{move || i18n.t("cmdpal.empty")}</div> }.into_any();
                            }
                            items
                                .into_iter()
                                .enumerate()
                                .map(|(idx, it)| {
                                    let active = idx == selected.get_untracked();
                                    let icon = item_icon(&it.kind);
                                    let title = it.title.clone();
                                    let subtitle = it.subtitle.clone();
                                    let invoke = invoke_cb;
                                    let it_for_click = it.clone();
                                    view! {
                                        <div
                                            class=move || if active { "cmdpal-item active" } else { "cmdpal-item" }
                                            on:mouseenter=move |_| selected.set(idx)
                                            on:click=move |_| invoke.run(it_for_click.clone())>
                                            <span class="cmdpal-icon">{icon}</span>
                                            <span class="cmdpal-text">
                                                <span class="cmdpal-title">{title}</span>
                                                <span class="cmdpal-subtitle">{subtitle}</span>
                                            </span>
                                        </div>
                                    }
                                })
                                .collect::<Vec<_>>()
                                .into_any()
                        }}
                    </div>
                </div>
            </div>
        </Show>
    }
}

/// 「执行选中项」的纯函数版本。
///
/// 不写在 view! 闭包里——闭包若捕获 `navigate` 后多次复用，会被推断成
/// FnOnce（impl Fn + Clone 的具体类型被 move 走就拿不回来）。改成函数
/// 让调用处每次都重新拿到新 clone 的 navigate。
///
/// 第二个形参 `current_slug` 也用 impl trait 接收，避免捕获闭包。
#[allow(clippy::too_many_arguments)]
fn do_invoke(
    item: PaletteItem,
    navigate: impl Fn(&str, leptos_router::NavigateOptions) + Clone + 'static,
    open: RwSignal<bool>,
    query: RwSignal<String>,
    selected: RwSignal<usize>,
    current_slug: impl Fn() -> String + Clone + 'static,
) {
    match item.kind {
        ItemKind::Workspace(slug) => {
            navigate(&format!("/{slug}"), Default::default());
        }
        ItemKind::Entry(_ws_id, code) => {
            let slug = current_slug();
            if !slug.is_empty() {
                navigate(&format!("/{slug}/entry/{code}"), Default::default());
            }
        }
        ItemKind::Command(cmd) => match cmd {
            PaletteCommand::GoWorkspaces => {
                navigate("/workspaces", Default::default());
            }
            PaletteCommand::GoAccount => {
                navigate("/account", Default::default());
            }
            PaletteCommand::GoAdmin => {
                navigate("/admin", Default::default());
            }
            PaletteCommand::GoSettings => {
                let slug = current_slug();
                if !slug.is_empty() {
                    navigate(&format!("/{slug}/settings"), Default::default());
                }
            }
            PaletteCommand::ThemeToggle => {
                if let Some(ctrl) = use_context::<ThemeController>() {
                    ctrl.cycle();
                }
            }
        },
    }
    open.set(false);
    query.set(String::new());
    selected.set(0);
}

/// 给一项配图标。
fn item_icon(kind: &ItemKind) -> AnyView {
    match kind {
        ItemKind::Command(PaletteCommand::GoWorkspaces) => ic_folder().into_any(),
        ItemKind::Command(PaletteCommand::GoAccount) => ic_profile().into_any(),
        ItemKind::Command(PaletteCommand::GoAdmin) => ic_logout().into_any(),
        ItemKind::Command(PaletteCommand::GoSettings) => ic_setting().into_any(),
        ItemKind::Command(PaletteCommand::ThemeToggle) => ic_tag().into_any(),
        ItemKind::Workspace(_) => ic_folder().into_any(),
        ItemKind::Entry(_, _) => ic_history().into_any(),
    }
}
