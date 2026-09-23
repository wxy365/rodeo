use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use serde_json::Value;

use crate::frontend::graphql_client::{
    get_recent_colors, logout, push_recent_color, AuditLog, Member,
};
use crate::frontend::icons::{ic_check, ic_copy, ic_logout, ic_profile, ic_setting};
use crate::frontend::use_auth;

/// 账号的展示名：优先姓名，缺失时退回邮箱。
pub fn member_label(m: &Member) -> String {
    if m.name.trim().is_empty() {
        m.email.clone()
    } else {
        m.name.clone()
    }
}

/// 账号选择器的候选列表上限：够用又不至于把弹层撑得比屏幕还高。
const ACCOUNT_HITS: usize = 8;

/// 账号选择器：只能从工作空间成员里挑，带按姓名 / 邮箱的搜索。
///
/// 值是账号 id（服务端按 id 校验成员身份），展示用姓名。选中即回调 `on_pick`，
/// 由调用方决定是立即落库（LabelRow）还是只写本地草稿（新建 Entry）。
#[component]
pub fn AccountPicker(
    /// 候选成员（当前工作空间成员表）。
    members: Vec<Member>,
    /// 已选账号 id，空串表示未设置；只用于展示。
    current: String,
    on_pick: Callback<String>,
) -> impl IntoView {
    let query = RwSignal::new(String::new());
    let label = members
        .iter()
        .find(|m| m.account_id == current)
        .map(member_label)
        // 成员已被移出工作空间时仍要把 id 显示出来，而不是假装没设置过。
        .unwrap_or_else(|| current.clone());
    view! {
        <div style="position:relative">
            <input class="inp" style="width:150px" placeholder="搜索账号…" prop:value=query
                on:input=move |ev| query.set(event_target_value(&ev)) />
            {move || {
                let q = query.get().trim().to_lowercase();
                if q.is_empty() {
                    return ().into_any();
                }
                let hits: Vec<Member> = members
                    .iter()
                    .filter(|m| {
                        m.name.to_lowercase().contains(&q) || m.email.to_lowercase().contains(&q)
                    })
                    .take(ACCOUNT_HITS)
                    .cloned()
                    .collect();
                if hits.is_empty() {
                    return view! { <div class="lblhint"><div class="mut">"没有匹配的成员"</div></div> }
                        .into_any();
                }
                view! {
                    <div class="lblhint">
                        {hits
                            .into_iter()
                            .map(|m| {
                                let id = m.account_id.clone();
                                view! {
                                    <div class="lblhint-it" on:click=move |_| {
                                        on_pick.run(id.clone());
                                        query.set(String::new());
                                    }>
                                        <span>{member_label(&m)}</span>
                                        <span class="mut">{m.email.clone()}</span>
                                    </div>
                                }
                            })
                            .collect::<Vec<_>>()}
                    </div>
                }
                .into_any()
            }}
            <div style="font-size:12px">
                {if current.is_empty() {
                    view! { <span class="mut">"未设置"</span> }.into_any()
                } else {
                    view! { <span class="chip">{label.clone()}</span> }.into_any()
                }}
            </div>
        </div>
    }
}

/// 时间类型的展示格式：常用格式下拉框 + 「自定义…」逃生口。
///
/// 值一律是常规表示法（`YYYY-MM-DD`），不是 Go 布局；库里可能还存着历史 Go 布局，
/// 这类值不在常用列表里，直接落进自定义输入态，用户可原样保存或改掉。
#[component]
pub fn FormatSelect(
    /// date / time / datetime
    value_type: String,
    format: RwSignal<String>,
    #[prop(optional)] disabled: bool,
) -> impl IntoView {
    let presets = crate::golayout::presets(&value_type);
    let custom = RwSignal::new({
        let cur = format.get_untracked();
        !cur.is_empty() && !presets.iter().any(|p| *p == cur)
    });
    let vt_ph = value_type.clone();
    // 「（默认）」选项把该类型的默认模式写在标签里：窄表格里比另起一行提示更省地方。
    let default_opt = format!("（默认 {}）", crate::golayout::default_pattern(&value_type));
    view! {
        <div style="display:flex;gap:6px;align-items:center">
            {move || {
                // 每次重渲染都要一份新字符串，闭包才是 FnMut。
                let default_opt = default_opt.clone();
                if custom.get() {
                    view! {
                        <input class="inp" style="width:180px" disabled=disabled
                            placeholder=crate::golayout::default_pattern(&vt_ph)
                            prop:value=move || format.get()
                            on:input=move |ev| format.set(event_target_value(&ev)) />
                        <button class="btn sm" type="button" disabled=disabled on:click=move |_| {
                            custom.set(false);
                            format.set(String::new());
                        }>"常用格式"</button>
                    }
                        .into_any()
                } else {
                    view! {
                        <select class="inp" style="width:180px" disabled=disabled
                            prop:value=move || format.get()
                            on:change=move |ev| {
                                let v = event_target_value(&ev);
                                // 「自定义…」只切输入态，先不动格式值，把当前值留给用户改。
                                if v == CUSTOM_FORMAT {
                                    custom.set(true);
                                } else {
                                    format.set(v);
                                }
                            }>
                            <option value="">{default_opt}</option>
                            {presets
                                .iter()
                                .map(|p| {
                                    let sel = move || format.get() == *p;
                                    view! { <option value=*p selected=sel>{*p}</option> }
                                })
                                .collect::<Vec<_>>()}
                            <option value=CUSTOM_FORMAT>"自定义…"</option>
                        </select>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

/// 「自定义…」选项的哨兵值：常规表示法里只有字母和连字符，下划线开头的串撞不上。
const CUSTOM_FORMAT: &str = "__custom__";

/// 标签定义的值默认值输入：给 Entry 添加标签时预填这个值。
///
/// 状态用 JSON 表示，形状与条目打标时写库的 `LabelValue` 线上形态一致
/// （枚举单值 / 字符串型 → String，多选 → Array，布尔 → Bool，数值 → Number，
/// 时间 / 邮箱 → String），`Value::Null` 表示「没设默认值」。Null 型标签没有值可设，
/// 它在新建 Entry 表单里靠「自动勾选」体现，故此处不渲染控件。
#[component]
pub fn DefaultValueInput(
    value_type: String,
    #[prop(optional)] multi: bool,
    #[prop(optional)] enum_values: Vec<String>,
    /// 时间类型的展示格式（常规表示法），空串表示该类型默认格式。
    #[prop(optional)] format: String,
    /// 账号型的候选成员。
    #[prop(optional)] members: Vec<Member>,
    value: RwSignal<Value>,
    #[prop(optional)] disabled: bool,
) -> impl IntoView {
    let vt = value_type.clone();
    let control = match vt.as_str() {
        "null" => view! { <span class="mut">"—"</span> }.into_any(),
        "boolean" => {
            let cur = move || match value.get() {
                Value::Bool(true) => "true".to_string(),
                Value::Bool(false) => "false".to_string(),
                _ => String::new(),
            };
            view! {
                <select class="inp" style="width:110px" disabled=disabled prop:value=cur
                    on:change=move |ev| {
                        value
                            .set(match event_target_value(&ev).as_str() {
                                "true" => Value::Bool(true),
                                "false" => Value::Bool(false),
                                _ => Value::Null,
                            });
                    }>
                    <option value="">"（未设置）"</option>
                    <option value="true">"是"</option>
                    <option value="false">"否"</option>
                </select>
            }
                .into_any()
        }
        "enum" if multi => {
            let cur = move || value_to_string(&value.get());
            view! {
                <input class="inp" style="width:180px" placeholder="逗号分隔，留空为不设置"
                    disabled=disabled prop:value=cur
                    on:input=move |ev| {
                        let items: Vec<Value> = event_target_value(&ev)
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .map(Value::String)
                            .collect();
                        value
                            .set(
                                if items.is_empty() { Value::Null } else { Value::Array(items) },
                            );
                    } />
            }
                .into_any()
        }
        "enum" => {
            let opts = enum_values.clone();
            let cur = move || value_to_string(&value.get());
            view! {
                <select class="inp" style="width:150px" disabled=disabled prop:value=cur
                    on:change=move |ev| {
                        let v = event_target_value(&ev);
                        value
                            .set(if v.is_empty() { Value::Null } else { Value::String(v) });
                    }>
                    <option value="">"（未设置）"</option>
                    {opts
                        .into_iter()
                        .map(|o| view! { <option value=o.clone()>{display_enum_value(&o)}</option> })
                        .collect::<Vec<_>>()}
                </select>
            }
                .into_any()
        }
        "integer" | "float" | "currency" => {
            let vt_num = vt.clone();
            let cur = move || match value.get() {
                Value::Number(n) => n.to_string(),
                _ => String::new(),
            };
            view! {
                <input class="inp" style="width:110px" type="number" disabled=disabled
                    prop:value=cur
                    on:input=move |ev| {
                        let v = event_target_value(&ev);
                        if v.trim().is_empty() {
                            value.set(Value::Null);
                            return;
                        }
                        let parsed = if vt_num == "integer" {
                            v.parse::<i64>().ok().map(|i| Value::Number(i.into()))
                        } else {
                            v.parse::<f64>()
                                .ok()
                                .and_then(serde_json::Number::from_f64)
                                .map(Value::Number)
                        };
                        // 解析失败（如只输入了 「-」）保持原值，避免写入写不进去的东西。
                        if let Some(n) = parsed {
                            value.set(n);
                        }
                    } />
            }
                .into_any()
        }
        "date" | "time" | "datetime" => {
            let vt_time = vt.clone();
            let stored = Some(format.as_str());
            let layout = crate::golayout::resolve(stored, crate::golayout::default_go(&vt));
            let hint =
                crate::golayout::display_pattern(stored, crate::golayout::default_pattern(&vt_time));
            view! {
                <input class="inp" style="width:180px" disabled=disabled placeholder=hint
                    prop:value=move || value_to_string(&value.get())
                    on:input=move |ev| {
                        let v = event_target_value(&ev);
                        if v.trim().is_empty() {
                            value.set(Value::Null);
                        } else if crate::golayout::parse(&layout, &v).is_some() {
                            value.set(Value::String(v));
                        }
                    } />
            }
                .into_any()
        }
        "account" => {
            let picked = Callback::new(move |id: String| value.set(Value::String(id)));
            let current = move || value_to_string(&value.get());
            view! {
                <AccountPicker members=members.clone() current=current() on_pick=picked />
            }
                .into_any()
        }
        // string / email：纯文本，落库前服务端还会再校验一次。
        _ => view! {
            <input class="inp" style="width:180px" placeholder="留空为不设置" disabled=disabled
                prop:value=move || value_to_string(&value.get())
                on:input=move |ev| {
                    let v = event_target_value(&ev);
                    value.set(if v.is_empty() { Value::Null } else { Value::String(v) });
                } />
        }
        .into_any(),
    };
    view! {
        <div style="display:flex;gap:6px;align-items:center">
            <span class="mut" style="white-space:nowrap">"默认值"</span>
            {control}
        </div>
    }
}

/// 圆形头像，取首字符展示。
#[component]
pub fn Avatar(#[prop(into)] text: String, #[prop(optional)] large: bool) -> impl IntoView {
    let cls = if large { "av lg" } else { "av" };
    let ch = text.chars().next().unwrap_or('?').to_string();
    view! { <span class=cls>{ch}</span> }
}

/// 右上角的用户头像 + 悬停菜单：`个人信息` 与 `退出`。
///
/// 登出逻辑（清本地令牌 → 回登录页 → 通知服务端吊销）就住在这里，两个页面共用一份。
/// 各页若再抄一份，改登出流程时必然会漏掉一处。
///
/// 菜单展开靠 CSS 的 `:hover` 与 `:focus-within` 两条：只用 `:hover` 的话键盘用户
/// 永远打不开这个菜单。
#[component]
pub fn AvatarMenu() -> impl IntoView {
    let auth = use_auth();
    let navigate = use_navigate();

    let nav_profile = navigate.clone();
    let go_profile = move |_| nav_profile("/account", Default::default());

    let nav_admin = navigate.clone();

    let nav_logout = navigate.clone();
    let do_logout = move |_| {
        auth.user.set(None);
        nav_logout("/login", Default::default());
        spawn_local(async move {
            logout().await;
        });
    };

    // `Avatar` 的 `text` 是 `String`（带 `#[prop(into)]`），不是信号，所以这里用一个
    // 响应式闭包重新构造它，而不是把一个 `Signal` 传进去。
    view! {
        <div class="avatar-menu">
            {move || {
                let u = auth.user.get();
                let text = u
                    .map(|u| if u.name.trim().is_empty() { u.email } else { u.name })
                    .unwrap_or_default();
                view! { <Avatar text=text /> }
            }}
            <div class="avatar-drop">
                <button class="mi" on:click=go_profile>{ic_profile()}"个人信息"</button>
                // 非管理员整项不渲染，而不是置灰：一个点不动的管理入口只会招来「为什么点不动」。
                {move || {
                    let nav_admin = nav_admin.clone();
                    auth.user.get().is_some_and(|u| u.is_admin).then(|| view! {
                        <button class="mi" on:click=move |_| nav_admin("/admin", Default::default())>{ic_setting()}"系统管理"</button>
                    })
                }}
                <button class="mi" on:click=do_logout>{ic_logout()}"退出"</button>
            </div>
        </div>
    }
}

/// 标签值 → 展示字符串。
pub fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        // 多值标签（如多选 Enum）按「逗号 + 空格」拼接各元素，而不是吐 JSON。
        Value::Array(a) => a.iter().map(value_to_string).collect::<Vec<_>>().join(", "),
        _ => v.to_string(),
    }
}

/// date / time / datetime 三类中，`format` 缺省或恰为默认格式时，原生 HTML
/// 控件就能表达该值；只有自定义格式才需要退回文本输入。
pub fn is_native_time_layout(vt: &str, format: Option<&str>) -> bool {
    let layout = crate::golayout::resolve(format, crate::golayout::default_go(vt));
    layout == crate::golayout::DATE_LAYOUT
        || layout == crate::golayout::TIME_LAYOUT
        || layout == crate::golayout::DATETIME_LAYOUT
}

/// 存储串（Go 默认布局）→ 原生控件的值。
///
/// 原生 date / time / datetime-local 只认浏览器格式，喂存储串会被判定为非法值而显示成空。
pub fn to_native(vt: &str, s: &str) -> String {
    match vt {
        "datetime" => s.replacen(' ', "T", 1).get(..16).unwrap_or(s).to_string(),
        "time" => s.get(..5).unwrap_or(s).to_string(),
        _ => s.to_string(),
    }
}

/// 原生控件的值 → 存储串（Go 默认布局：时间类补齐秒、datetime 去 `T` 换空格）。
/// 空串返回 `None`，由调用方决定是「不写」还是「移除」。
pub fn from_native(vt: &str, s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    Some(match vt {
        "datetime" => format!("{}:00", s.replacen('T', " ", 1)).get(..19)?.to_string(),
        "time" => format!("{s}:00").get(..8)?.to_string(),
        _ => s.to_string(),
    })
}

/// 内置枚举值 → 友好展示（InProgress → In progress 等）。
pub fn display_enum_value(v: &str) -> String {
    match v {
        "InProgress" => "In progress".to_string(),
        "WontFix" => "Wont fix".to_string(),
        other => other.to_string(),
    }
}

/// Task/Bug 等状态值 → chip 颜色类。
pub fn status_chip(value: &str) -> &'static str {
    match value {
        "InProgress" => "c-doing",
        "Done" | "Fixed" => "c-done",
        "WontFix" | "Archived" => "c-wont",
        "Open" => "c-open",
        _ => "c-open",
    }
}

/// 优先级值 → chip 颜色类。
pub fn priority_chip(value: &str) -> &'static str {
    match value {
        "P0" | "P1" => "c-p0",
        "P2" => "c-p2",
        "P3" => "c-p3",
        _ => "c-open",
    }
}

/// 依据 label_name 选择状态/优先级 chip 类。
pub fn label_chip_class(label_name: &str, value: &str) -> &'static str {
    if label_name.eq_ignore_ascii_case("priority") || label_name.eq_ignore_ascii_case("优先级") {
        priority_chip(value)
    } else {
        status_chip(value)
    }
}

/// 角色 → chip 颜色类。
pub fn role_chip_class(role: &str) -> &'static str {
    match role.to_ascii_lowercase().as_str() {
        "owner" => "c-done",
        "maintainer" => "c-doing",
        "worker" => "c-open",
        "reader" => "dim",
        _ => "dim",
    }
}

/// 角色显示名。
pub fn role_label(role: &str) -> String {
    match role.to_ascii_lowercase().as_str() {
        "owner" => "Owner".to_string(),
        "maintainer" => "Maintainer".to_string(),
        "worker" => "Worker".to_string(),
        "reader" => "Reader".to_string(),
        other => other.to_string(),
    }
}

/// 角色是否达到指定等级。与后端 `WorkspaceRole` 的声明顺序一致
/// （owner > maintainer > worker > reader）。
/// 后端 `as_str()` 返回小写，这里仍照 `role_label` 的做法归一化一次，
/// 免得上游哪天改成大写时静默退化成 Reader。
pub fn role_at_least(role: &str, min: &str) -> bool {
    let rank = |r: &str| match r.to_ascii_lowercase().as_str() {
        "owner" => 3,
        "maintainer" => 2,
        "worker" => 1,
        _ => 0,
    };
    rank(role) >= rank(min)
}

/// 未登录（浏览器端无 token，或非浏览器环境一律视为未登录）。
pub fn logged_out() -> bool {
    if cfg!(target_arch = "wasm32") {
        crate::frontend::graphql_client::get_token().is_none()
    } else {
        true
    }
}

/// RFC3339 → 本地时区的 (年, 月, 日, 时, 分, 秒)；解析失败返回 `None`。
///
/// 服务端的时间戳一律是 UTC（`Utc::now().to_rfc3339()`），直接切字符串会把 UTC 当本地时间展示。
/// 本地时区偏移只有浏览器知道，所以只有 wasm 端能换算；SSR 阶段这些时间戳还没取到，
/// 走下面各自的切片回退。
#[cfg(target_arch = "wasm32")]
fn local_parts(rfc: &str) -> Option<(u32, u32, u32, u32, u32, u32)> {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(rfc.trim()));
    if js_sys::Date::get_time(&d).is_nan() {
        return None;
    }
    Some((
        js_sys::Date::get_full_year(&d),
        js_sys::Date::get_month(&d) + 1,
        js_sys::Date::get_date(&d),
        js_sys::Date::get_hours(&d),
        js_sys::Date::get_minutes(&d),
        js_sys::Date::get_seconds(&d),
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn local_parts(_rfc: &str) -> Option<(u32, u32, u32, u32, u32, u32)> {
    None
}

/// RFC3339 时间 → 本地时区 "YYYY-MM-DD HH:MM"。
pub fn short_time(at: &str) -> String {
    if let Some((y, mo, d, h, mi, _)) = local_parts(at) {
        return format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}");
    }
    let s = at.replace('T', " ");
    s.chars().take(16).collect()
}

/// RFC3339 → 本地时区的 `2006-01-02 15:04:05`。
pub fn fmt_datetime(rfc: &str) -> String {
    if let Some((y, mo, d, h, mi, s)) = local_parts(rfc) {
        return format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}");
    }
    let s = rfc.trim();
    match (s.get(..10), s.get(11..19)) {
        (Some(d), Some(t)) => format!("{d} {t}"),
        (Some(d), None) => d.to_string(),
        _ => s.to_string(),
    }
}

/// 字节数 → 人类可读。附件行用它，免得把 5242880 这种数直给用户。
pub fn human_size(bytes: i64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let n = bytes.max(0);
    let b = n as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{n} B")
    }
}

/// 审计 action → 中文标签。
pub fn action_label(action: &str) -> &'static str {
    match action {
        "EntryCreated" => "创建",
        "EntryUpdated" => "更新详情",
        "EntryDeleted" => "删除",
        "LabelingSet" => "设置标签",
        "LabelingRemoved" => "移除标签",
        "LabelSchemaCreated" => "创建标签定义",
        "LabelSchemaUpdated" => "更新标签定义",
        "LabelSchemaDeleted" => "删除标签定义",
        "ViewCreated" => "创建视图",
        "ViewUpdated" => "更新视图",
        "ViewDeleted" => "删除视图",
        "EntryArchived" => "归档",
        "EntryUnarchived" => "取消归档",
        "MemberInvited" => "邀请成员",
        "MemberJoined" => "接受邀请",
        "InviteDeclined" => "拒绝邀请",
        "InviteRevoked" => "撤销邀请",
        "RoleChanged" => "变更成员角色",
        "MemberRemoved" => "移除成员",
        "WorkspaceUpdated" => "更新工作空间",
        "WorkspaceDeleted" => "删除工作空间",
        "WorkspaceRestored" => "恢复工作空间",
        "WorkspaceCreated" => "创建工作空间",
        "RuleCreated" => "创建规则",
        "RuleUpdated" => "更新规则",
        "RuleDeleted" => "删除规则",
        "RuleApplied" => "规则触发",
        "CommentCreated" => "发表评论",
        "CommentUpdated" => "编辑评论",
        "CommentDeleted" => "删除评论",
        "AttachmentUploaded" => "上传附件",
        "AttachmentDeleted" => "删除附件",
        _ => "变更",
    }
}

/// 审计 before/after 快照 → 一句话「具体改了什么」。
///
/// 两个快照都是 JSON 对象字符串（`serde_json::to_string` 出来的资源快照）。创建 / 删除
/// 各只有一边，写成「创建「标题」」；两边都在时逐字段对比，只报实际变化的字段。
pub fn audit_change(before: Option<&str>, after: Option<&str>) -> String {
    let parse = |s: Option<&str>| -> Option<Value> {
        let s = s?;
        serde_json::from_str::<Value>(s).ok()
    };
    let b = parse(before);
    let a = parse(after);
    let bobj = b.as_ref().and_then(Value::as_object);
    let aobj = a.as_ref().and_then(Value::as_object);
    match (bobj, aobj) {
        (None, None) => "—".to_string(),
        (None, Some(ao)) => format!("创建「{}」", subject(ao)),
        (Some(bo), None) => format!("删除「{}」", subject(bo)),
        (Some(bo), Some(ao)) => diff(bo, ao),
    }
}

/// 资源快照 → 一个能指代它的短标签。
fn subject(o: &serde_json::Map<String, Value>) -> String {
    for k in ["title", "name", "label_name", "value", "email", "account_id"] {
        if let Some(v) = o.get(k) {
            let s = show(v);
            if s != "—" {
                return s;
            }
        }
    }
    "（无标题）".to_string()
}

fn diff(b: &serde_json::Map<String, Value>, a: &serde_json::Map<String, Value>) -> String {
    let null = Value::Null;
    // 先按 after 的字段顺序，再补上只存在于 before 的字段（被移除的字段）。
    let mut keys: Vec<&String> = a.keys().collect();
    for k in b.keys() {
        if !a.contains_key(k) {
            keys.push(k);
        }
    }
    let mut parts = Vec::new();
    for k in keys {
        let bv = b.get(k).unwrap_or(&null);
        let av = a.get(k).unwrap_or(&null);
        if bv == av {
            continue;
        }
        parts.push(format!("{}: {} → {}", field_label(k), show(bv), show(av)));
    }
    if parts.is_empty() {
        "无字段变化".to_string()
    } else {
        parts.join("；")
    }
}

/// 快照字段名 → 中文；未在映射表里的字段直接沿用原名。
fn field_label(k: &str) -> String {
    match k {
        "title" => "标题",
        "detail" => "详情",
        "name" => "名称",
        "slug" => "地址",
        "description" => "描述",
        "deleted_at" => "删除时间",
        "value" => "值",
        "label_name" => "标签",
        "value_type" => "值类型",
        "enum_values" => "可选值",
        "color" => "颜色",
        "value_colors" => "值颜色",
        "query" => "条件",
        "columns" => "列",
        "sort" => "排序",
        "is_shared" => "共享",
        "title_colors" => "标题颜色规则",
        "role" => "角色",
        "account_id" => "成员",
        other => other,
    }
    .to_string()
}

/// 展示一个 JSON 值：字符串去掉引号，其余保持紧凑 JSON；过长则截断。
fn show(v: &Value) -> String {
    match v {
        Value::Null => "—".to_string(),
        Value::String(s) => clip(s),
        other => clip(&other.to_string()),
    }
}

fn clip(s: &str) -> String {
    let one_line = s.replace('\n', " ");
    if one_line.chars().count() <= 40 {
        one_line
    } else {
        let mut t: String = one_line.chars().take(40).collect();
        t.push('…');
        t
    }
}

/// 值类型显示名。
pub fn value_type_label(vt: &str) -> String {
    match vt {
        "null" => "Null",
        "boolean" => "Boolean",
        "integer" => "Integer",
        "float" => "Float",
        "string" => "String",
        "enum" => "Enum",
        "date" => "日期",
        "time" => "时间",
        "datetime" => "日期时间",
        "currency" => "金额",
        "email" => "邮箱",
        "account" => "账号",
        other => other,
    }
    .to_string()
}

/// 复制文本到系统剪贴板。非 wasm 目标下为空实现，便于 `cargo check` 通过。
#[cfg(target_arch = "wasm32")]
pub fn copy_to_clipboard(text: &str) {
    let Some(win) = leptos::web_sys::window() else {
        return;
    };
    // 丢弃 Promise 不影响写入：它是已排入队列的异步任务。
    let _ = win.navigator().clipboard().write_text(text);
}

#[cfg(not(target_arch = "wasm32"))]
pub fn copy_to_clipboard(_text: &str) {}

/// 编码 + 复制按钮。复制成功后按钮短暂变成对勾。
///
/// 表格、详情面板、全屏页头三处同款，外观由 `.codecell` / `.code` / `.codecopy` 承载——
/// 一并带上 `.code` 是为了在表格之外的场景也拿到等宽小字。
#[component]
pub fn CodeCopy(#[prop(into)] code: Signal<String>) -> impl IntoView {
    let copied = RwSignal::new(false);
    view! {
        <span class="codecell code">
            <span>{move || code.get()}</span>
            <button class="ibtn codecopy" title="复制编码" on:click=move |_| {
                copy_to_clipboard(&code.get_untracked());
                copied.set(true);
                set_timeout(
                    move || copied.set(false),
                    std::time::Duration::from_millis(1200),
                );
            }>
                {move || if copied.get() {
                    ic_check().into_any()
                } else {
                    ic_copy().into_any()
                }}
            </button>
        </span>
    }
}

/// 页签条：`tabs` 是 (键, 显示名) 列表，`active` 持有当前键。
/// 只渲染与切换，显示什么内容由调用方按 `active` 自己分发。
#[component]
pub fn TabBar(
    tabs: &'static [(&'static str, &'static str)],
    active: RwSignal<String>,
) -> impl IntoView {
    view! {
        <div class="dtabs">
            {tabs
                .iter()
                .map(|(key, label)| {
                    let for_class = *key;
                    let for_click = *key;
                    view! {
                        <button
                            class=move || if active.get() == for_class { "on" } else { "" }
                            on:click=move |_| active.set(for_click.to_string())
                        >{*label}</button>
                    }
                })
                .collect::<Vec<_>>()}
        </div>
    }
}

/// 条目历史时间线：把工作空间的审计日志过滤到某条目后按时间倒序渲染。
/// 详情的「历史」页签与全屏页共用——两处原本各写一遍，且都只做同一件事。
#[component]
pub fn AuditTimeline(logs: RwSignal<Vec<AuditLog>>, code: Signal<String>) -> impl IntoView {
    view! {
        {move || {
            let c = code.get();
            let mine: Vec<AuditLog> = logs
                .get()
                .into_iter()
                .filter(|l| l.resource_id == c)
                .collect();
            if mine.is_empty() {
                view! { <div class="mut">"暂无记录"</div> }.into_any()
            } else {
                view! {
                    {mine
                        .into_iter()
                        .map(|l| {
                            view! {
                                <div class="tl">
                                    <span class="t">{short_time(&l.at)}</span>
                                    <span>{action_label(&l.action)}</span>
                                    <span class="mut" style="font-size:12px">
                                        {audit_change(l.before.as_deref(), l.after.as_deref())}
                                    </span>
                                </div>
                            }
                        })
                        .collect::<Vec<_>>()}
                }
                .into_any()
            }
        }}
    }
}

/// 标准色板：与原生调色盘并排、可直接点选的一组常用色。
pub const PRESET_COLORS: [&str; 10] = [
    "#ef4444", // 红
    "#f97316", // 橙
    "#eab308", // 黄
    "#22c55e", // 绿
    "#14b8a6", // 青
    "#3b82f6", // 蓝
    "#6366f1", // 靛
    "#a855f7", // 紫
    "#ec4899", // 粉
    "#6b7280", // 灰
];

/// 未设色时原生控件里显示的那个色，与各调用点此前的默认值一致。
const COLOR_FALLBACK: &str = "#3b82f6";

/// 颜色选择器：原生调色盘 + 标准色 + 最近使用色，标签基础色、值色行、标题颜色
/// 规则三处共用一份。
///
/// 原生调色盘照旧负责调自定义色——网页改不了系统调色盘里的内容，「标准色直接
/// 点选」只能落在控件旁边这一排色点上。色点常显会把表格撑宽，所以整组折叠进
/// 悬停浮层，平时只占一个调色盘按钮的宽度。
#[component]
pub fn ColorPick(
    /// 当前颜色，空串表示未设置。
    #[prop(into)]
    value: Signal<String>,
    /// 任何一种点选（标准色 / 最近使用 / 调色盘）都走这里，落不落库由调用方决定。
    #[prop(into)]
    on_pick: Callback<String>,
    /// 当前工作空间：最近使用色按它隔离。
    #[prop(into)]
    ws_id: Signal<String>,
    #[prop(optional)] small: bool,
    #[prop(optional)] disabled: bool,
    #[prop(optional, into)] title: Option<String>,
) -> impl IntoView {
    // 初值空表、挂载后再从 localStorage 恢复：SSR 与 hydrate 两趟的 DOM 才一致。
    let recent = RwSignal::new(Vec::<String>::new());
    Effect::new_sync(move |_| {
        if cfg!(target_arch = "wasm32") {
            recent.set(get_recent_colors(&ws_id.get_untracked()));
        }
    });

    // 记一笔用过的颜色。标准色板里已有的不记——否则最近使用只会是标准色的副本，
    // 这一组是留给调色盘里调出来的自定义色的。
    let record = move |c: String| {
        if cfg!(target_arch = "wasm32") && !PRESET_COLORS.contains(&c.as_str()) {
            recent.set(push_recent_color(&ws_id.get_untracked(), &c));
        }
        on_pick.run(c);
    };

    let dot = move |c: String| {
        let bg = c.clone();
        let label = c.clone();
        let sel = c.clone();
        let pick = c;
        view! {
            <button
                type="button"
                class="cpick-dot"
                class:sel=move || value.get().eq_ignore_ascii_case(&sel)
                style=format!("background:{bg}")
                title=label
                disabled=disabled
                // 鼠标按下时不让色点拿到焦点。`:focus-within` 是留给键盘用户的——没有它
                // 键盘就摸不到这排色点；但鼠标点过的色点也会一直攥着焦点，于是指针移开
                // 浮层也收不起来。键盘激活（Tab + 回车）不触发 mousedown，所以那条路不受影响。
                on:mousedown=|ev| ev.prevent_default()
                on:click=move |_| record(pick.clone())
            ></button>
        }
    };

    view! {
        <div class="cpick" class:off=disabled>
            <input
                type="color"
                class="sw"
                class:sm=small
                title=title
                disabled=disabled
                prop:value=move || {
                    let c = value.get();
                    if c.is_empty() { COLOR_FALLBACK.to_string() } else { c }
                }
                on:input=move |ev| record(event_target_value(&ev))
            />
            <div class="cpick-pop">
                <div class="cpick-group" title="标准色">
                    {PRESET_COLORS.iter().map(|c| dot(c.to_string())).collect::<Vec<_>>()}
                </div>
                {move || {
                    let list = recent.get();
                    (!list.is_empty())
                        .then(|| {
                            view! {
                                <div class="cpick-group cpick-sep" title="最近使用">
                                    {list.into_iter().map(|c| dot(c.to_string())).collect::<Vec<_>>()}
                                </div>
                            }
                        })
                }}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_change_reports_only_changed_fields() {
        let b = r#"{"title":"旧","detail":"同","value":"Open"}"#;
        let a = r#"{"title":"新","detail":"同","value":"Open"}"#;
        assert_eq!(audit_change(Some(b), Some(a)), "标题: 旧 → 新");
    }

    #[test]
    fn audit_change_summarizes_create_and_delete() {
        assert_eq!(
            audit_change(None, Some(r#"{"title":"条目0"}"#)),
            "创建「条目0」"
        );
        assert_eq!(
            audit_change(Some(r#"{"name":"看板"}"#), None),
            "删除「看板」"
        );
        assert_eq!(audit_change(None, None), "—");
    }

    #[test]
    fn audit_change_marks_added_and_removed_fields() {
        let b = r#"{"a":1}"#;
        let a = r#"{"b":2}"#;
        assert_eq!(audit_change(Some(b), Some(a)), "b: — → 2；a: 1 → —");
    }
}
