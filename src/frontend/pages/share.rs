//! 公开访问的匿名只读 Entry 分享页（路由 `/share/{token}`）。
//!
//! 拉取数据走 `entryFullByShareToken`（`/api/graphql`），不传 Authorization；
//! 无 token / 解析失败统一显示「该分享链接无效或已撤销」。
//!
//! 页面镜像 `EntryFullScreen` 的整体布局：顶部 CodeCopy + 标题 +
//! 「返回首页」按钮；正文区是 `TinyEditor readonly`；下方挂评论列表
//! （只读渲染）；右侧面板是元信息、标签（只读 chip）、附件列表（无上传入口）。
//! **不**渲染「保存」、「分享」、「删除」、Relations 页签、AuditTimeline——
//! 这些都依赖登入态或与匿名读者无关的工作空间内部结构。

use leptos::prelude::*;
use leptos_router::hooks::use_params_map;

use crate::frontend::components::{fmt_datetime, Avatar, CodeCopy};
use crate::frontend::graphql_client::{
    entry_full_by_share_token, AccountBrief, EntryFullPublic, MemberPublic, SharedAttachment,
    SharedComment,
};
use crate::frontend::i18n::{use_i18n, I18nController};
use crate::frontend::icons::{ic_back, ic_comment, ic_folder, ic_tag};
use crate::frontend::tiny_editor::TinyEditor;

// `LocalResource` 只在客户端触发 fetcher，SSR 永远 pending；share 页是
// 匿名访问，SSR 没必要渲染——浏览器 hydrate 后立即发请求把数据拉回来。
// `Resource::new` 在 wasm32 上要 `Send + 'static`，但 gloo-net 的
// `Request::send()` 持有 `Rc<RefCell<...>>`，是 `!Send`，
// 所以走 `LocalResource` 是 leptos 0.8 给出的「wasm-only fetch」官方姿势。

/// 把 GraphQL `AuthorName`/`MemberPublic` 等公开投影统一成字符串显示。
fn name_or_dash(name: &str) -> &str {
    if name.is_empty() {
        "—"
    } else {
        name
    }
}

fn author_name(by: &Option<AccountBrief>) -> String {
    by.as_ref()
        .map(|a| name_or_dash(&a.name).to_string())
        .unwrap_or_else(|| "—".to_string())
}

/// 评论列表项视图：与 `comment_list.rs::CommentList` 那种「拉数据 → 渲染」
/// 的双层结构不同，分享页数据是 GraphQL 一次拿齐的，直接渲染即可。
#[component]
fn comment_item(comment: SharedComment) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    let author = name_or_dash(&comment.author_name).to_string();
    view! {
        <div class="c-item">
            <Avatar text=author.clone() />
            <div class="c-main">
                <div class="c-head">
                    <span class="c-name">{author}</span>
                    <span class="t">{comment.created_at.clone()}</span>
                    {comment.edited.then(|| view! {
                        <span class="mut">{move || i18n.t("comment.edited")}</span>
                    })}
                </div>
                <div class="c-body" inner_html=crate::frontend::tiny_editor::delta_to_html(&comment.body)></div>
            </div>
        </div>
    }
}

/// 附件行：保持下载链接、文件名、大小；不暴露上传者。复用
/// `attachment_list.rs` 的样式 class 以让 CSS 共享。
#[component]
fn attachment_row(a: SharedAttachment) -> impl IntoView {
    let url = a.url.clone();
    let filename = a.filename.clone();
    let size = a.size;
    view! {
        <div class="att">
            <span class="ic">{ic_folder()}</span>
            <a href=url download=filename>{filename.clone()}</a>
            <span class="sz">{crate::frontend::components::human_size(size)}</span>
        </div>
    }
}

#[component]
pub fn SharedEntry() -> impl IntoView {
    let i18n: I18nController = use_i18n();
    // `use_params_map()` 在 effect 闭包里调用，否则初始挂载时 Router context
    // 还没建立。`let token = ...` 一次拿到再传给后续构造。
    let token = use_params_map()
        .get_untracked()
        .get("token")
        .unwrap_or_default();

    let data: LocalResource<Result<Option<EntryFullPublic>, String>> =
        LocalResource::new(move || {
            let t = token.clone();
            async move { entry_full_by_share_token(&t).await }
        });

    // 「返回首页」：匿名读者登入态未必有，跳到 `/` 让泳道自己分流。
    let go_home = Callback::new(move |_: ()| {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(win) = web_sys::window() {
                let _ = win.location().set_href("/");
            }
        }
    });

    let workspace_link = move |slug: String| -> String { format!("/{slug}") };

    view! {
        <div class="page page-wide shared-entry-page">
            <Suspense fallback=move || view! {
                <div class="panel" style="text-align:center;padding:32px">
                    <span class="mut">{move || i18n.t("share_view.loading")}</span>
                </div>
            }>
                {move || match data.get() {
                    None => view! {
                        <div class="panel" style="text-align:center;padding:32px">
                            <span class="mut">{move || i18n.t("share_view.loading")}</span>
                        </div>
                    }.into_any(),
                    Some(Ok(Some(full))) => {
                        // 顶层信号：让 <CodeCopy> 取到的是稳定字符串而不是泄露闭包类型。
                        let code_str = full.entry.code.clone();
                        let title_str = full.entry.title.clone();
                        let detail_str = full.entry.detail.clone();
                        let slug_link = full.workspace_slug.clone();
                        let workspace_url = workspace_link(slug_link);
                        // `Signal::derive` 要一个返回值的零参闭包——
                        // 把字符串克隆搬进闭包体，重读时再克隆出来传出去。
                        let code_for_signal = code_str.clone();
                        let entry_code_signal = Signal::derive(move || code_for_signal.clone());
                        // 仅展示用：把 workspace_name / slug / created_by 等元
                        // 信息借静态字符串塞进 view!。
                        let entry_title = title_str;
                        let entry_detail = detail_str;
                        let entry_created = full.entry.created_at.clone();
                        let entry_updated = full.entry.updated_at.clone();
                        let entry_created_by =
                            author_name(&full.entry.created_by_account);
                        let workspace_name = full.workspace_name.clone();
                        let comments_empty = full.comments.is_empty();
                        let comments_count = full.comments.len();
                        let attachments_empty = full.attachments.is_empty();
                        let attachments_count = full.attachments.len();
                        let i18n_comments = i18n.clone();
                        view! {
                            <div class="panel entry-top">
                                <button class="btn" on:click=move |_| go_home.run(())>
                                    {ic_back()}
                                    {move || i18n.t("share_view.go_home")}
                                </button>
                                <CodeCopy code=entry_code_signal />
                                <h1 class="entry-title">{entry_title.clone()}</h1>
                                // 分享页禁止编辑标题——直接渲染纯文本。
                                <span class="chip dim">
                                    <a href=workspace_url.clone() target="_blank" rel="noopener noreferrer">
                                        {workspace_name.clone()}
                                    </a>
                                </span>
                            </div>

                            <div class="entry-layout">
                                <div class="entry-main">
                                    <div class="panel entry-editor" style="padding:0;overflow:hidden">
                                        <TinyEditor
                                            initial=entry_detail.clone()
                                            entry_code=Signal::derive(move || code_str.clone())
                                            on_change=Callback::new(|_d: String| {})
                                            on_uploaded=Callback::new(|_| {})
                                            members=Vec::new()
                                            readonly=true
                                        />
                                    </div>

                                    <div class="panel entry-comments">
                                        <div class="grp-h">
                                            {ic_comment()}
                                            {format!(
                                                "{}（{}）",
                                                i18n_comments.t("comment.section_title"),
                                                comments_count,
                                            )}
                                        </div>
                                        {if comments_empty {
                                            view! {
                                                <div class="mut">{move || i18n.t("comment.empty")}</div>
                                            }.into_any()
                                        } else {
                                            view! {
                                                <div class="c-list">
                                                    {full.comments.into_iter().map(|c| {
                                                        view! { <CommentItem comment=c /> }
                                                    }).collect::<Vec<_>>()}
                                                </div>
                                            }.into_any()
                                        }}
                                    </div>
                                </div>

                                <aside class="panel entry-side">
                                    <div class="dmeta">
                                        <div>
                                            <span class="mut">{move || i18n.t("entry.created_by")}</span>
                                            {entry_created_by.clone()}
                                        </div>
                                        <div>
                                            <span class="mut">{move || i18n.t("entry.created_at")}</span>
                                            {fmt_datetime(&entry_created)}
                                        </div>
                                        <div>
                                            <span class="mut">{move || i18n.t("entry.updated_at")}</span>
                                            {fmt_datetime(&entry_updated)}
                                        </div>
                                    </div>

                                    <div>
                                        <div class="grp-h">
                                            {ic_tag()}
                                            {move || i18n.t("entry.labels_hint")}
                                        </div>
                                        // 只读标签：直接读数据渲染，不依赖共享组件的
                                        // 取数逻辑，避免贡献双倍的 me / role 请求。
                                        <ReadOnlyLabels
                                            labels=full.entry.labels.clone()
                                            schemas=full.label_schemas.clone()
                                        />
                                    </div>

                                    <div>
                                        <div class="grp-h">
                                            {ic_folder()}
                                            {format!(
                                                "{}（{}）",
                                                i18n_comments.t("attachment.section_title"),
                                                attachments_count,
                                            )}
                                        </div>
                                        {if attachments_empty {
                                            view! { <div class="mut">{move || i18n.t("attachment.empty")}</div> }.into_any()
                                        } else {
                                            view! {
                                                <div class="att-list">
                                                    {full.attachments.into_iter().map(|a| {
                                                        view! { <AttachmentRow a /> }
                                                    }).collect::<Vec<_>>()}
                                                </div>
                                            }.into_any()
                                        }}
                                    </div>

                                    <div>
                                        <div class="grp-h">
                                            {move || i18n.t("share_view.members_heading")}
                                        </div>
                                        <MembersGrid members=full.members />
                                    </div>
                                </aside>
                            </div>
                        }.into_any()
                    },
                    // token 失效 / 已撤销 / Entry 删了 → 统一文案，无存在性区分。
                    Some(Ok(None)) | Some(Err(_)) => view! {
                        <div class="panel" style="text-align:center;padding:32px">
                            <p>{move || i18n.t("share_view.not_found")}</p>
                            <button class="btn pri" on:click=move |_| go_home.run(())>
                                {move || i18n.t("share_view.go_home")}
                            </button>
                        </div>
                    }.into_any(),
                }}
            </Suspense>
        </div>
    }
}

/// 只读标签视图：把 GraphQL 返回的 `labels` + `schemas` 渲染成纯 chip，
/// 不依赖 `LabelEditor` 的「写」路径。这样可以在公开分享页呈现「这条
/// Entry 上打过哪些标签」而不暴露任何「添加 / 删除 / 编辑」入口。
/// `schemas` 用来取配色（值色 / 基础色），与 `LabelEditor` 的颜色逻辑同源；
/// 没有 schema 时降级为无色 chip。
#[component]
fn ReadOnlyLabels(
    labels: Vec<crate::frontend::graphql_client::Labeling>,
    schemas: Vec<crate::frontend::graphql_client::LabelSchema>,
) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    if labels.is_empty() {
        return view! { <div class="mut">{i18n.t("share_view.no_labels")}</div> }.into_any();
    }
    view! {
        <div class="lbledit-readonly">
            {labels.into_iter().map(|l| {
                let schema = schemas.iter().find(|s| s.name == l.label_name).cloned();
                let name = l.label_name.clone();
                let title = schema
                    .as_ref()
                    .map(|s| s.title.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| name.clone());
                let color = schema.as_ref().and_then(|s| s.color.clone());
                let value = l.value.clone();
                // 简化展示：「title = value」或裸 title（无值 / boolean）。
                let vt = schema
                    .as_ref()
                    .map(|s| s.value_type.as_str())
                    .unwrap_or("");
                let value_str = if matches!(value, serde_json::Value::Null) {
                    String::new()
                } else if vt == "boolean" {
                    String::new()
                } else {
                    value.to_string()
                };
                let body = if value_str.is_empty() {
                    title.clone()
                } else {
                    format!("{title}={value_str}")
                };
                let style = color
                    .map(|c| format!(
                        "border-color:{c};background:color-mix(in srgb, {c} 12%, transparent);color:{c}"
                    ))
                    .unwrap_or_default();
                view! {
                    <div class="lblrow" style=style>
                        <span class="k">{body}</span>
                    </div>
                }
            }).collect::<Vec<_>>()}
        </div>
    }
    .into_any()
}

/// 工作空间成员名头像列表。`MemberPublic` 只有 id + name，没有邮箱/角色——
/// 略缩为头像 + 名字一行的紧凑样式。
#[component]
fn MembersGrid(members: Vec<MemberPublic>) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    if members.is_empty() {
        return view! { <div class="mut">{i18n.t("share_view.no_members")}</div> }.into_any();
    }
    view! {
        <div class="shared-members">
            {members.into_iter().map(|m| {
                let name = m.name.clone();
                view! {
                    <div class="shared-member">
                        <Avatar text=name.clone() />
                        <span>{name}</span>
                    </div>
                }
            }).collect::<Vec<_>>()}
        </div>
    }
    .into_any()
}
