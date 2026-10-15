use leptos::ev::SubmitEvent;
use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};

use crate::frontend::attachment_list::AttachmentList;
use crate::frontend::comment_list::CommentList;
use crate::frontend::i18n::{use_i18n, I18nController};
use crate::frontend::components::{
    chip_glyph, display_enum_value, fmt_datetime, from_native, is_native_time_layout,
    label_chip_class, logged_out, member_label, short_time, to_native, value_to_string,
    AccountPicker, AuditTimeline, CodeCopy, ColorPick, GlyphPos, TabBar,
};
use crate::frontend::graphql_client::{
    archive_entry, archived_entries, audit_logs, create_entry, create_view, delete_entry,
    delete_view, entry, format_view_query, get_sidebar_collapsed, label_schemas, members,
    parse_view_query, query_all_entries, query_entries, set_labeling,
    set_labelings, set_sidebar_collapsed, set_view_timeline, summarize_entries, unarchive_entry,
    update_entry, update_view, views, workspace_ai_config, workspace_by_slug, AccountBrief,
    AuditLog, Entry, LabelSchema, Labeling, Member, NamedPrompt, View, ViewSort, ViewTimeline,
    Workspace,
};
use crate::frontend::icons::{
    ic_back, ic_close, ic_comment, ic_copy, ic_folder, ic_full, ic_help, ic_save, ic_search,
    ic_setting, ic_share, ic_tag, ic_undo,
};
use crate::frontend::label_editor::LabelEditor;
use crate::frontend::query_eval;
use crate::frontend::relations_panel::RelationsPanel;
use crate::frontend::timeline::TimelineView;
use crate::frontend::tiny_editor::TinyEditor;
use crate::frontend::use_auth;
use crate::frontend::view_filter::with_text;
use serde_json::Value;

use crate::frontend::agent_panel::provide_agent_context;

/// 内置元数据关键字 → 本地化展示名。恒定并入 `/` 候选，与视图里有哪些标签无关
/// （`src/domain/query.rs` 的 `RESERVED_FIELDS` 是同一份名单）。展示名通过 i18n 在
/// 使用处即时解析，避免 `const` 里写死中文。
fn builtin_fields(i18n: &I18nController) -> [(String, String); 7] {
    [
        ("Code".to_string(), i18n.t("entry.col_code")),
        ("Title".to_string(), i18n.t("entry.col_title")),
        ("Detail".to_string(), i18n.t("entry.col_detail")),
        ("CreatedBy".to_string(), i18n.t("entry.col_created_by")),
        ("CreatedAt".to_string(), i18n.t("entry.col_created_at")),
        ("UpdatedBy".to_string(), i18n.t("entry.col_updated_by")),
        ("UpdatedAt".to_string(), i18n.t("entry.col_updated_at")),
    ]
}

/// Entry 元信息列在 `View.columns` 里的 sentinel。`@@` 前缀既能在视觉上与标签名清晰
/// 区分，也不会与合法的 label name 冲突（label name 由 `domain/label.rs` 校验）。
/// `EntryTable` 看到这几个名字就改去渲染对应的元信息单元格，而不是去 schemas 里
/// 找同名标签。`service/view.rs::is_metadata_column` 是同份名单的服务端镜像。
const COLUMN_CREATED_BY: &str = "@@createdBy";
const COLUMN_CREATED_AT: &str = "@@createdAt";
const COLUMN_UPDATED_BY: &str = "@@updatedBy";
const COLUMN_UPDATED_AT: &str = "@@updatedAt";

/// 全部 4 个元信息 sentinel 的本地化展示名 / 排序字段名。表头与单元格渲染、
/// 「配置视图」弹窗里的复选框都从这里取，避免四处散落的字符串。
fn metadata_columns(i18n: &I18nController) -> [(&'static str, &'static str, String); 4] {
    [
        (COLUMN_CREATED_BY, "createdBy", i18n.t("entry.col_created_by")),
        (COLUMN_CREATED_AT, "createdAt", i18n.t("entry.col_created_at")),
        (COLUMN_UPDATED_BY, "updatedBy", i18n.t("entry.col_updated_by")),
        (COLUMN_UPDATED_AT, "updatedAt", i18n.t("entry.col_updated_at")),
    ]
}

/// 给定列名是否为元信息 sentinel；返回 `Some(sort_field)` 供 `sortable_th` 用。
fn metadata_sort_field(name: &str) -> Option<&'static str> {
    match name {
        COLUMN_CREATED_BY => Some("createdBy"),
        COLUMN_CREATED_AT => Some("createdAt"),
        COLUMN_UPDATED_BY => Some("updatedBy"),
        COLUMN_UPDATED_AT => Some("updatedAt"),
        _ => None,
    }
}

/// 视图「标题颜色规则」编辑行状态。`RwSignal` 便于逐字段就地更新；
/// 全字段 Copy，便于在 `For` 的多个事件闭包间复用。
#[derive(Clone, Copy)]
struct TitleRule {
    id: u32,
    /// 条件表达式文本（提交前用 `parse_view_query` 转 AST）。
    expr: RwSignal<String>,
    /// #rrggbb 取色。
    color: RwSignal<String>,
    /// 预填时的原始表达式；与 `expr` 相同时可复用 `cached_ast`，免去重复解析。
    cached_expr: RwSignal<String>,
    /// 预填时已解析好的查询 AST；`Value::Null` 表示无缓存（新加的空行）。
    cached_ast: RwSignal<Value>,
}

/// 「新建 Entry」表单里的待写入标签。与 `LabelEditor` 不同，此时条目还不存在，
/// 无法逐次 upsert，故先在这里收集，创建成功后再逐个落库。
#[derive(Clone)]
struct DraftLabel {
    name: String,
    title: String,
    value_type: &'static str,
    /// 是否多值（目前用于多选 Enum）。
    multi: bool,
    /// date / time / datetime 的自定义 Go 布局；`None` 表示默认布局（可用原生控件表达）。
    format: Option<String>,
    enum_values: RwSignal<Vec<String>>,
    /// 字符串 / 数值 / 枚举选中的原始文本；空串表示「未设置」。
    text: RwSignal<String>,
    /// 多选 Enum 选中的值集合。
    many: RwSignal<Vec<String>>,
    /// 布尔标签三态：`None` 未设置，`Some(b)` 明确写入 true / false。
    /// 用三态而非复选框，否则「未勾选」无法与「明确设为假」区分。
    flag: RwSignal<Option<bool>>,
}

impl DraftLabel {
    /// `preset=false`：一律空着等用户填（用户主动打标签时用）。
    fn from_schema(s: LabelSchema) -> Self {
        Self::new(s, false)
    }

    /// `preset=true`：用标签定义的默认值预填（新建 Entry 的自动集用它）。
    /// 没有默认值的值类型仍然留空——不填即「本次不写这个标签」。
    fn from_schema_preset(s: LabelSchema) -> Self {
        Self::new(s, true)
    }

    fn new(s: LabelSchema, preset: bool) -> Self {
        let value_type = match s.value_type.as_str() {
            "null" => "null",
            "enum" => "enum",
            "boolean" => "boolean",
            "integer" => "integer",
            "float" => "float",
            "date" => "date",
            "time" => "time",
            "datetime" => "datetime",
            "currency" => "currency",
            "email" => "email",
            "account" => "account",
            _ => "string",
        };
        // 无值标签的默认值序列化出来就是 JSON null，与「没配」无从区分；
        // 但自动集里的无值标签意图明确——「打上」，故按类型判定。
        let default = if preset && value_type != "null" {
            s.default_value.clone()
        } else {
            Value::Null
        };
        // 多选 Enum / Account 的默认值是数组，填进 `many`；其余类型都能用字符串表达。
        // Account 多选也是把账号 id 用字符串数组表达——草稿阶段不区分两者，
        // 直接 to_value 时按 self.multi 走。
        let (mut text, many, flag) = match &default {
            Value::Null => {
                // 自动集里的无值标签默认「打上」。
                let flag = (preset && value_type == "null").then_some(true);
                (String::new(), Vec::new(), flag)
            }
            Value::Bool(b) => (String::new(), Vec::new(), Some(*b)),
            Value::Array(a) => (String::new(), a.iter().map(value_to_string).collect(), None),
            other => (value_to_string(other), Vec::new(), None),
        };
        // 默认值是存储串，而原生控件只认浏览器格式，直接喂会被判非法而显示成空；
        // 自定义布局走文本输入，保持存储串原样（与 `to_value` 的换算方向一致）。
        if matches!(value_type, "date" | "time" | "datetime")
            && is_native_time_layout(value_type, s.format.as_deref())
        {
            text = to_native(value_type, &text);
        }
        Self {
            name: s.name,
            title: s.title,
            value_type,
            // 先读 `multi` / `format`，再把 `enum_values` 移进信号。
            multi: s.multi,
            format: s.format,
            enum_values: RwSignal::new(s.enum_values),
            text: RwSignal::new(text),
            many: RwSignal::new(many),
            flag: RwSignal::new(flag),
        }
    }

    /// 用户填了值才返回 `Some`；空输入一律视为「本次不写这个标签」。
    fn to_value(&self) -> Option<Value> {
        match self.value_type {
            // 无值标签：勾上即写 `null`，用来表示「打上了」。
            "null" => self
                .flag
                .get_untracked()
                .filter(|b| *b)
                .map(|_| Value::Null),
            "boolean" => self.flag.get_untracked().map(Value::Bool),
            // 多选 Enum：整体写成一个字符串数组；一个都没选则视为未设置。
            "enum" if self.multi => {
                let many = self.many.get_untracked();
                (!many.is_empty())
                    .then_some(Value::Array(many.into_iter().map(Value::String).collect()))
            }
            "enum" => {
                let v = self.text.get_untracked();
                (!v.is_empty()).then_some(Value::String(v))
            }
            // 多选 Account：和多选 Enum 共用 `many` 缓冲，写时统一表达成字符串数组。
            "account" if self.multi => {
                let ids: Vec<String> = self
                    .many
                    .get_untracked()
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                (!ids.is_empty())
                    .then_some(Value::Array(ids.into_iter().map(Value::String).collect()))
            }
            "integer" => self
                .text
                .get_untracked()
                .trim()
                .parse::<i64>()
                .ok()
                .map(|i| Value::Number(i.into())),
            "float" | "currency" => self
                .text
                .get_untracked()
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number),
            // 时间类默认布局用原生控件，写下的是浏览器格式（time 为 "14:30"，
            // datetime-local 为 "2026-09-15T14:30"），落库前补成 Go 存储布局；
            // 自定义布局走文本输入，用户给的已是存储串，落到下面的原样上报。
            "time" | "datetime"
                if is_native_time_layout(self.value_type, self.format.as_deref()) =>
            {
                from_native(self.value_type, &self.text.get_untracked()).map(Value::String)
            }
            _ => {
                let v = self.text.get_untracked();
                (!v.trim().is_empty()).then_some(Value::String(v))
            }
        }
    }
}

/// 新建 Entry 时自动打上的标签集合：当前视图的列 ∪ 视图表达式里用到的标签。
fn auto_label_names(columns: Vec<String>, query: &Value) -> Vec<String> {
    let mut out = columns;
    for name in query_eval::collect_label_names(query) {
        if !out.iter().any(|n| *n == name) {
            out.push(name);
        }
    }
    out
}

/// 时间轴配置下拉的候选。`time` 为真给日期 / 时间 / 日期时间型标签
/// （起止各选一个），为假给账号型（相关人）。
fn label_options(schemas: Vec<LabelSchema>, time: bool) -> Vec<impl IntoView> {
    schemas
        .into_iter()
        .filter(|s| {
            if time {
                matches!(s.value_type.as_str(), "date" | "time" | "datetime")
            } else {
                s.value_type == "account"
            }
        })
        .map(|s| {
            let (name, title) = (s.name, s.title);
            view! { <option value=name>{title}</option> }
        })
        .collect()
}

#[component]
pub fn WorkspaceMain() -> impl IntoView {
    let params = use_params_map();
    let slug = move || params.get_untracked().get("slug").unwrap_or_default();
    let navigate = use_navigate();
    let auth = use_auth();
    let i18n: I18nController = use_i18n();
    let refresh = RwSignal::new(0u32);
    let data: RwSignal<Option<Result<(Workspace, Vec<Entry>, Vec<LabelSchema>), String>>> =
        RwSignal::new(None);
    let schemas = RwSignal::new(Vec::<LabelSchema>::new());
    // 成员表：Account 型标签的候选，以及表格里把账号 id 显示成姓名。
    let ws_members = RwSignal::new(Vec::<Member>::new());
    // 评论组件查角色用；与 data 同源，避免多打一次 workspace 请求。
    let ws_id = Signal::derive(move || {
        data.get()
            .and_then(|r| r.ok())
            .map(|(w, _, _)| w.id)
            .unwrap_or_default()
    });
    let ws_name = RwSignal::new(String::new());
    // 行高亮信号：点击标题或其他列都会设它，仅控制表格行的 `.sel` 背景。
    let selected = RwSignal::new(String::new());
    // 右侧详情面板信号：只有点击标题（或键盘上下移动 / 新建条目成功）才设它，
    // 控制 `<EntryPanel code=...>` 显示哪一条。点其他列不应该打开面板。
    let panel_code = RwSignal::new(String::new());
    let show_new = RwSignal::new(false);
    let new_title = RwSignal::new(String::new());
    // 「新建 Entry」表单里的标题输入框引用：表单从关到开那一刻把焦点交过去，
    // 用户不再需要再点一次输入框。
    let new_title_ref: NodeRef<Input> = NodeRef::new();
    // 新建表单里待写入的标签值；打开表单时按当前 workspace 的标签 schema 重建。
    let new_labels = RwSignal::new(Vec::<DraftLabel>::new());
    let error = RwSignal::new(None::<String>);

    // ---- 批量设置标签 ----
    // 表格里勾选的条目 code。数据重载（翻页/换视图/刷新）时清空，避免选中的行
    // 已经不在当前列表里却仍被写入。
    let batch_selected = RwSignal::new(Vec::<String>::new());
    // 持久多选模式：右键 Entry 表格行序号（`row-no`）开启。
    // 模式下点击行只切 batch_selected、不开面板；ESC 或点表格外区域退出。
    // 退出时批量操作按钮栏也跟着隐藏，避免「没有入口却看见按钮」的怪态。
    let multi_select_mode = RwSignal::new(false);
    let show_batch = RwSignal::new(false);
    // 复用 batch_selected 作为 AgentPanel「选中条目列表」上下文（同一份 RwSignal,
    // 与批量设置标签 / AI 总结 / 归档按钮保持一致）。视图 id 的 authoritative 信号
    // 是下面 `let active_id = ...` 那行,这里只声明提供位置——见
    // `provide_agent_context` 调用站点。
    let batch_labels = RwSignal::new(Vec::<DraftLabel>::new());
    let batch_error = RwSignal::new(None::<String>);
    let batch_busy = RwSignal::new(false);

    // ---- AI 总结 ----
    // 场景 / 语气下拉数据在打开弹窗时才拉，和「已归档」弹窗一个路数：
    // 没打开过的用户不该为这个功能付一次请求。
    let show_ai = RwSignal::new(false);
    let ai_loading = RwSignal::new(false);
    let ai_scenarios = RwSignal::new(Vec::<NamedPrompt>::new());
    let ai_tones = RwSignal::new(Vec::<NamedPrompt>::new());
    let ai_scenario = RwSignal::new(String::new());
    let ai_tone = RwSignal::new(String::new());
    let ai_busy = RwSignal::new(false);
    let ai_error = RwSignal::new(None::<String>);
    // Agent 副作用（view_query_replace / view_search_replace）落地时,服务端的
    // parseViewQuery 偶尔会拒绝 LLM 回传的表达式——以前只打 console.warn,
    // 用户看不到提示,视图又没改,会以为 agent 失灵。这里挂一条短提示,5s
    // 后清掉。独立于 ai_error(那个是 AI 总结弹窗的错)。
    let agent_warning: RwSignal<Option<String>> = RwSignal::new(None);
    // 自定义 toast: 设置文字,等 5s 自动清空。两次连续触发时把计时器取消
    // 重设,免得前一条被新一条在屏幕中段覆盖。
    #[cfg(target_arch = "wasm32")]
    {
        let warn_for_timer = agent_warning;
        Effect::new(move |_| {
            if warn_for_timer.get().is_some() {
                let target = warn_for_timer;
                spawn_local(async move {
                    gloo_timers::future::TimeoutFuture::new(5_000).await;
                    // 仅当 5s 间没有新覆盖才清空——避免新消息被旧 timer 误清。
                    if target.get_untracked().is_some() {
                        target.set(None);
                    }
                });
            }
        });
    }

    // ---- 归档 ----
    // 已归档条目：打开「已归档」弹窗时拉取，恢复一条后就地移除，不整表重拉。
    let archived_open = RwSignal::new(false);
    let archived_list = RwSignal::new(Vec::<Entry>::new());
    let archived_error = RwSignal::new(None::<String>);
    let archived_busy = RwSignal::new(false);

    // ---- 布局状态 ----
    // 侧栏收缩：初始 false，挂载后在 Effect 内从 localStorage 恢复，避免 SSR/hydrate 不一致。
    let sidebar_collapsed = RwSignal::new(false);
    Effect::new_sync(move |_| {
        if cfg!(target_arch = "wasm32") {
            sidebar_collapsed.set(get_sidebar_collapsed());
        }
    });

    // ---- Agent 副作用 → 列表刷新 ----
    // SSE 推过来的 `SideEffectHint`（entry / labeling / comment 等）只要让 `tick`
    // 涨一格，主视图就要重查条目列表：用户改完才看到「AI 帮我新建的那一条」是体验底线。
    // 不按 domain 过滤——标签或评论的写入通常会顺带 bump 条目 `updated_at`，列表列
    // 不刷就显不一致。
    if let Some(eff) = use_context::<crate::frontend::agent_side_effects::AgentSideEffects>() {
        Effect::new(move |_| {
            eff.tick.track();
            refresh.update(|n| *n += 1);
        });
    }

    // ---- 筛选查询状态 ----
    let query_ast = RwSignal::new(serde_json::json!({ "and": [] }));
    // 已落库的视图查询条件基线。排序改动会自动落库（见 `persist_sort`），所以不再进基线——
    // 否则每点一次表头，「保存视图」按钮都会无意义地亮起来。
    // 基础视图没有这个按钮（它上面的过滤只作临时用途），但基线仍要维护——「视图配置」
    // 保存后会用它重新对齐。
    let saved_baseline: RwSignal<Value> = RwSignal::new(serde_json::json!({ "and": [] }));
    // 基线表达式的文本形态，与 `saved_baseline` 同步；用于「重置为默认」按钮
    // 把表达式输入框（expr_text）一并还原——免去按需调用 `format_view_query`。
    let saved_baseline_expr: RwSignal<String> = RwSignal::new(String::new());
    let expr_text = RwSignal::new(String::new());
    // 输入 `/` 时弹出的标签候选列表开关。
    let hint_open = RwSignal::new(false);
    // 光标前是「时间型键 + 比较运算符 + 空白」时，浮出原生时间控件的状态：
    // 控件类型 + 值该插入的字节位置（即当前文本末尾）。
    let time_pick = RwSignal::new(None::<(TimeKind, usize)>);
    // 表达式语法帮助弹窗开关。
    let expr_help = RwSignal::new(false);
    // 本视图命中的条目实际带过的标签名（跨分页去重），即 `/` 的候选集。
    let view_label_names = RwSignal::new(Vec::<String>::new());
    let ad_hoc_text = RwSignal::new(String::new());
    let refresh_view = RwSignal::new(0u32);

    // 视图查询反射：AI 调 queryEntries / searchEntries 命中后，把表达式 / 关键词
    // 回填到当前的表达式输入框 / 全文输入框。走 server-side parseViewQuery
    // 保证 AST 与 server 端完全同构——不在前端复刻 parser。位置放在信号定义
    // 之后，确保 effect 闭包捕获的是最终信号。
    if let Some(eff_ai) =
        use_context::<crate::frontend::agent_side_effects::AgentSideEffects>()
    {
        let expr_text_for_ai = expr_text;
        let query_ast_for_ai = query_ast;
        let ad_hoc_text_for_ai = ad_hoc_text;
        let refresh_view_for_ai = refresh_view;
        let data_for_ai = data;
        let warning_for_ai = agent_warning;
        // AgentSideEffects 在 App 层 provide，跨路由切换持久化；workspace_main
        // 每次挂载都会拿到「上一次 workspace 留下的 hint」。若直接应用,新 ws 的
        // expr_text 会被旧 ws 的 view_query_replace 覆盖。在挂载时刻快照
        // tick,只接受 mount 之后新到的 hint,切 ws 自然不会带副作用过来。
        let tick_at_mount = eff_ai.tick.get_untracked();
        Effect::new(move |_| {
            let cur_tick = eff_ai.tick.get();
            if cur_tick <= tick_at_mount {
                return;
            }
            let Some(hint) = eff_ai.last.get() else { return };
            match hint.domain.as_str() {
                "view_query_replace" => {
                    let Some(payload) = hint.payload.clone() else { return };
                    let Some(ws_id) = data_for_ai
                        .get()
                        .and_then(|r| r.ok())
                        .map(|(w, _, _)| w.id)
                    else {
                        return;
                    };
                    spawn_local(async move {
                        match parse_view_query(&ws_id, &payload).await {
                            Ok(ast) => {
                                batch(move || {
                                    expr_text_for_ai.set(payload);
                                    query_ast_for_ai.set(ast);
                                    ad_hoc_text_for_ai.set(String::new());
                                    refresh_view_for_ai.update(|n| *n += 1);
                                });
                            }
                            Err(e) => {
                                // parse 失败:LMM 给的表达式 server 不接受。
                                // 用户需要知道这件事——不然视图没变,以为 agent 罢工。
                                // 顶部挂 5s 自消失的提示,console.warn 留给排查。
                                leptos::web_sys::console::warn_1(
                                    &i18n.tf("entry.errors.invalid_view_query", &[("err", &e.to_string())]).into(),
                                );
                                warning_for_ai.set(Some(i18n.tf("entry.errors.ai_view_query_rejected", &[("err", &e.to_string())])));
                            }
                        }
                    });
                }
                "view_search_replace" => {
                    let Some(payload) = hint.payload.clone() else { return };
                    batch(move || {
                        ad_hoc_text_for_ai.set(payload);
                        refresh_view_for_ai.update(|n| *n += 1);
                    });
                }
                _ => {}
            }
        });
    }
    // ---- 分页状态 ----
    let page_signal = RwSignal::new(1i64);
    let page_size: i64 = 20;
    let total_signal = RwSignal::new(0i64);
    // 请求序号：并发请求乱序返回时只接受最新一次的结果。
    let req_seq = RwSignal::new(0u32);

    // ---- 视图侧栏状态 ----
    let view_list = RwSignal::new(Vec::<View>::new());
    let active_view = RwSignal::new(None::<View>);
    // 侧栏高亮只需 id；与 active_view 一并更新，保持二者同步。
    let active_id = RwSignal::new(None::<String>);
    // URL 上的 `?view=<id>`：作为 active_id 的初始种子，让「从全屏页返回」
    // 和「浏览器刷新」都能恢复到上次选中的视图，而不是落到基础视图。
    // 一次性读，不订阅 location.search 变化——URL 同步由 open_entry / go_back
    // 主动写入，避免路由切换与 set_active 互相触发。
    let initial_view_id: Option<String> = use_query_map()
        .get()
        .get("view")
        .filter(|s| !s.is_empty());
    // 提供 AgentPanel 取用的上下文：当前视图 id + 表格勾选 codes。
    // 复用 batch_selected（与批量按钮同读同一份信号，行为一致）。
    let _ = provide_agent_context(active_id, batch_selected);
    // 视图导航栏右键菜单：(client_x, client_y, view_id)。None 表示关闭。
    let vmenu: RwSignal<Option<(i32, i32, String)>> = RwSignal::new(None);
    // ---- 时间轴视图 ----
    // 是否切到时间轴渲染。切换视图时归零（见 `set_active`）；配置被清掉时
    // `tl_active` 会自行变假，不必再额外同步。
    let timeline_mode = RwSignal::new(false);
    // 视图配置弹窗里的三个选择：起始 / 结束时间标签、相关人标签。
    // 空串分别表示「不启用」「不展示相关人」，与服务端「空串即清除」对齐。
    let config_tl_start = RwSignal::new(String::new());
    let config_tl_end = RwSignal::new(String::new());
    let config_tl_person = RwSignal::new(String::new());
    // 真正生效的开关：既要切到时间轴，当前视图也得确实配了。
    // 配置在弹窗里被清掉后，界面自动退回表格，不会卡在一条空轴上。
    let tl_active = Signal::derive(move || {
        timeline_mode.get() && active_view.get().is_some_and(|v| v.timeline.is_some())
    });
    // 选中视图变化时，播种 query_ast 并把 id/active_view 一并切换；id 未变则整体跳过。
    // Leptos 0.8 的 RwSignal::set 无相等短路，无条件写 active_view 会让 Effect（以
    // active_view / query_ast 为依赖）与 load_views 相互触发，形成无限刷新回环。
    let set_active = move |v: Option<View>| {
        let new_id = v.as_ref().map(|v| v.id.clone());
        if active_id.get_untracked() == new_id {
            return;
        }
        // 表达式输入框跟随视图：用服务端下发的 `queryExpr` 回填，避免前端复刻语法。
        let expr = v.as_ref().map(|v| v.query_expr.clone()).unwrap_or_default();
        // 同一批内改写所有相关 signal，Effect 只跑一次，避免旧视图/旧页码的并发请求乱序覆盖。
        batch(move || {
            saved_baseline.set(
                v.as_ref()
                    .map(|v| v.query.clone())
                    .unwrap_or_else(|| serde_json::json!({ "and": [] })),
            );
            saved_baseline_expr.set(expr.clone());
            query_ast.set(
                v.as_ref()
                    .map(|v| v.query.clone())
                    .unwrap_or_else(|| serde_json::json!({ "and": [] })),
            );
            active_id.set(new_id);
            active_view.set(v);
            expr_text.set(expr);
            hint_open.set(false);
            // 整段文本被换掉，旧的时间控件插入位置随之失效，一并收起。
            time_pick.set(None);
            // 切换视图回到第 1 页，避免旧的页码超出新视图总页数导致空表。
            page_signal.set(1);
            // 新视图未必配了时间轴，渲染模式跟着回普通视图。
            timeline_mode.set(false);
        });
    };
    let load_views = move |ws_id: String, initial: Option<String>| {
        spawn_local(async move {
            if let Ok(list) = views(&ws_id).await {
                // URL `?view=<id>` 作为种子：当前 active_id 在列表里找不到时，
                // 若 URL 指定的 id 仍存在，就用它；找不到才退回默认视图。
                // 覆盖三种场景：1) 从全屏页返回 / 刷新，active_id 是 None；
                // 2) 切换工作空间，旧 id 不在新列表里；
                // 3) 视图被删，URL 还挂着过期 id。
                let mut candidate = active_id.get_untracked();
                if !candidate
                    .as_ref()
                    .is_some_and(|id| list.iter().any(|v| &v.id == id))
                {
                    if let Some(ref id) = initial {
                        if list.iter().any(|v| &v.id == id) {
                            candidate = Some(id.clone());
                        }
                    }
                }
                let still_valid = candidate
                    .as_ref()
                    .is_some_and(|id| list.iter().any(|v| &v.id == id));
                if !still_valid {
                    set_active(list.first().cloned());
                } else if candidate.as_deref() != active_id.get_untracked().as_deref() {
                    // 候选变了（典型：从 URL 恢复成 X），用对应视图对象唤醒 active_view
                    // 等下游信号——直接改 active_id 不会触发 query_ast / expr_text 重置。
                    if let Some(id) = candidate.as_ref() {
                        if let Some(v) = list.iter().find(|v| &v.id == id).cloned() {
                            set_active(Some(v));
                        }
                    }
                }
                view_list.set(list);
            }
        });
    };
    let select_view = Callback::new(move |id: String| {
        if let Some(v) = view_list.get().into_iter().find(|v| v.id == id) {
            set_active(Some(v));
        }
    });
    let delete_view_cb = Callback::new(move |id: String| {
        spawn_local(async move {
            let _ = delete_view(&id).await;
            view_list.update(|l| l.retain(|v| v.id != id));
            if active_id.get().as_deref() == Some(id.as_str()) {
                set_active(view_list.get().first().cloned());
            }
        });
    });
    // 右键视图行弹出菜单：(id, client_x, client_y) 由叶子行给出；我们只负责把
    // 坐标塞进 `vmenu` 让浮层定位。基础视图 (`is_default`) 跳过该行为，已在
    // 叶子行的 contextmenu 闭包里就地过滤，避免每次右键都回查 view_list。
    let on_context_view = Callback::new(move |(id, x, y): (String, i32, i32)| {
        vmenu.set(Some((x, y, id)));
    });

    // 视图导航栏右键菜单：点菜单以外的位置即关闭。
    //
    // `.vmenu` div 自己 `on:mousedown=stop_propagation()`——菜单内的 mousedown
    // 不会冒到这里。能冒到这里的就是「点了菜单外」，闭包里直接清掉 vmenu 即可，
    // 不要再做任何"是否在菜单里"的推断：之前用 `target().closest(".vmenu")`
    // / `composed_path()` 走冒泡链判断，每个浏览器 quirk（Chrome 把 button 的
    // mousedown target 设成 Text 节点、shadow DOM 边界截断 path、闭包捕获已 dispose
    // 的信号导致 wasm unreachable …）都会让菜单被误关、按钮 click 链被拆断；
    // c7c9bcb / d8f6e81 / 5dbc7f3 三次提交都在围着这段判断修，仍然每次都复发。
    // 改架构：让菜单自己声明边界，窗口层只承担"菜单外 = 关"。
    //
    // `handle` 仍要存进 on_cleanup 里 remove：闭包捕获 `vmenu`，路由切换 / hydrate
    // 重建 Owner 时信号会 dispose，未撤的 window 监听器会在某次 mousedown 里抛
    // "reactive value … already been disposed"，wasm unreachable 吞掉后续 click，
    // 表现为"点了没反应"。
    #[cfg(target_arch = "wasm32")]
    {
        let close_vmenu = move |_ev: leptos::ev::MouseEvent| {
            if vmenu.get_untracked().is_some() {
                vmenu.set(None);
            }
        };
        let handle = window_event_listener(leptos::ev::mousedown, close_vmenu);
        on_cleanup(move || handle.remove());
    }

    // 把表达式输入框的内容解析成查询 AST 并应用（回车触发）。空输入即清空过滤。
    // 解析交给服务端 `parse_view_query`：语法与标签 schema 校验只有一份实现。
    let apply_expr = move || {
        let expr = expr_text.get_untracked();
        let Some(ws_id) = data
            .get()
            .and_then(|r| r.ok())
            .map(|(w, _, _)| w.id.clone())
        else {
            error.set(Some(i18n.t("entry.errors.workspace_not_loaded")));
            return;
        };
        spawn_local(async move {
            match parse_view_query(&ws_id, &expr).await {
                Ok(ast) => {
                    query_ast.set(ast);
                    page_signal.set(1);
                    error.set(None);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };
    // 选中候选标签：把最后一个 `/` 连同其后已输入的前缀替换成标签名，
    // 并补一个空格，方便紧接着输入运算符或下一个标签。
    let pick_label = move |name: String| {
        let cur = expr_text.get_untracked();
        expr_text.set(match cur.rfind('/') {
            Some(i) => format!("{}{name} ", &cur[..i]),
            None => format!("{name} "),
        });
        hint_open.set(false);
        time_pick.set(None);
    };

    // 「重置」按钮的判脏：AST 与基线不一致，或两个输入框里还残留未应用的文本。
    // 只看 AST 不够：用户在输入框里敲完未回车时 `query_ast` 还没变，但「重置」按钮该亮，
    // 否则用户想一键还原就只能挨个清输入框。
    // 对普通视图（即已保存表达式的视图）：基线非空时只看输入框是否非空会把
    // 刚加载完的视图也判脏——所以改为对比 expr_text 与 saved_baseline_expr。
    let view_dirty = move || {
        active_view.get().is_some()
            && (query_ast.get() != saved_baseline.get()
                || expr_text.get_untracked() != saved_baseline_expr.get()
                || !ad_hoc_text.get_untracked().trim().is_empty())
    };
    // 「保存视图」按钮的判脏：只比 AST——保存的语义是「把当前 AST 落到视图上」，
    // ad-hoc 全文搜索与未回车的表达式文本都不会被保存进视图，不该点亮这个按钮。
    let view_savable = move || {
        active_view.get().is_some() && query_ast.get() != saved_baseline.get()
    };

    // 排序自动落库，不再依赖「保存视图」按钮。两条约束：
    // 1) 只改排序——`query` 用视图已存的那份，不是界面上的 `query_ast`（那是临时过滤态，
    //    不该被一次点表头顺手固化）；其余字段原样回传。
    // 2) 基础视图不发请求——服务端本来就会忽略它的排序改动，排序在那上面是本地态。
    // 请求必须串行：`ViewService::update` 在服务端是整条记录覆盖（last-writer-wins），
    // 并发下发时若后发的短排序链先落地、先发的长链后落地，库里留下的是过期链，下次
    // 加载就静默丢掉后点的排序键。故同一时刻只允许一个请求在飞：飞行中的新点击只写进
    // `persist_pending`（只留最新一条，中间态已被更新的链取代），由当前请求完成后接力。
    let persist_busy = RwSignal::new(false);
    let persist_pending = RwSignal::new(None::<View>);
    let persist_sort = move |v: View| {
        if v.is_default {
            return;
        }
        if persist_busy.get_untracked() {
            persist_pending.set(Some(v));
            return;
        }
        persist_busy.set(true);
        spawn_local(async move {
            let mut next = Some(v);
            while let Some(v) = next {
                match update_view(
                    &v.id,
                    &v.name,
                    &v.query,
                    &v.sorts,
                    &v.columns,
                    v.is_shared,
                    &v.title_colors,
                )
                .await
                {
                    Ok(saved) => {
                        // 上一次失败可能留下横幅，成功后清掉。
                        error.set(None);
                        // 不回写 active_view：界面已按新排序查过一次，再写会多触发一轮查询。
                        view_list.update(|l| {
                            if let Some(slot) = l.iter_mut().find(|x| x.id == saved.id) {
                                *slot = saved.clone();
                            }
                        });
                    }
                    // 落库失败不回滚本地排序（列表已经按新排序显示），但要说出来。
                    Err(e) => error.set(Some(i18n.tf("entry.errors.sort_save_failed", &[("err", &e.to_string())]))),
                }
                // 接力最新待发项：取与清之间没有 await（wasm 单线程），读写是原子的。
                next = persist_pending.get_untracked();
                persist_pending.set(None);
            }
            persist_busy.set(false);
        });
    };

    // ---- 新建 / 另存为视图弹窗 ----
    let show_view_dialog = RwSignal::new(false);
    // 两个入口共用一个弹窗，标题与内容随入口变。
    let view_dialog_saveas = RwSignal::new(false);
    let view_name_input = RwSignal::new(String::new());
    let view_shared_input = RwSignal::new(false);
    let view_columns_input = RwSignal::new(Vec::<String>::new()); // 选中展示为列的标签 name
                                                                  // 要落库的条件与排序：「新建视图」给空条件，「另存为新视图」继承当前视图的排序链。
    let view_query_input = RwSignal::new(serde_json::json!({ "and": [] }));
    let view_sort_input = RwSignal::new(Vec::<ViewSort>::new());

    // ---- 视图配置弹窗（重命名 + 列配置）----
    let show_config_dialog = RwSignal::new(false);
    let config_name_input = RwSignal::new(String::new());
    let config_columns_input = RwSignal::new(Vec::<String>::new());
    let config_shared_input = RwSignal::new(false);
    // 弹窗内错误单独存放：页面级 error 渲染在弹窗遮罩之下，用户看不到。
    let dialog_error = RwSignal::new(None::<String>);
    // ---- 标题颜色规则编辑（视图配置弹窗内）----
    let config_rules = RwSignal::new(Vec::<TitleRule>::new());
    let next_rule_id = RwSignal::new(0u32);
    // 打开弹窗时逐个把已有规则的 AST 反格式化为表达式文本（服务端串行请求），
    // 期间禁用保存，避免把尚未回填、看似为空的条件误删。
    let rules_loading = RwSignal::new(false);
    let add_rule = move |_| {
        let id = next_rule_id.get_untracked();
        next_rule_id.set(id + 1);
        config_rules.update(|rows| {
            rows.push(TitleRule {
                id,
                expr: RwSignal::new(String::new()),
                color: RwSignal::new("#3b82f6".to_string()),
                cached_expr: RwSignal::new(String::new()),
                cached_ast: RwSignal::new(Value::Null),
            })
        });
    };

    // 复用入口：来自视图导航栏的「配置视图」右键菜单，以及（未来移除后的）其他地方。
    let open_config_for = Callback::new(move |id: String| {
        let Some(v) = view_list.get_untracked().into_iter().find(|v| v.id == id) else {
            error.set(Some(i18n.t("entry.errors.view_deleted")));
            return;
        };
        config_name_input.set(v.name.clone());
        // 「更新时间」在弹窗里默认勾选——配置视图打开时如果原 columns 没有它，
        // 就把它追加到勾选态。用户保存即落库；不保存只代表「看了一下默认是开的」，
        // 原 columns 一行不动。基础视图 columns 恒空，这条路径负责把弹窗预填成
        // 「默认勾选更新时间」，与「表格初始只有标题列」的需求配套。
        let mut cols = v.columns.clone();
        if !cols.iter().any(|c| c == COLUMN_UPDATED_AT) {
            cols.push(COLUMN_UPDATED_AT.to_string());
        }
        config_columns_input.set(cols);
        config_shared_input.set(v.is_shared);
        config_tl_start.set(
            v.timeline
                .as_ref()
                .map(|t| t.start.clone())
                .unwrap_or_default(),
        );
        config_tl_end.set(
            v.timeline
                .as_ref()
                .map(|t| t.end.clone())
                .unwrap_or_default(),
        );
        config_tl_person.set(
            v.timeline
                .as_ref()
                .and_then(|t| t.person.clone())
                .unwrap_or_default(),
        );
        dialog_error.set(None);
        let raw = v.title_colors.as_array().cloned().unwrap_or_default();
        let rows: Vec<TitleRule> = raw
            .iter()
            .enumerate()
            .map(|(i, r)| TitleRule {
                id: i as u32,
                expr: RwSignal::new(String::new()),
                color: RwSignal::new(
                    r.get("color")
                        .and_then(|c| c.as_str())
                        .unwrap_or("#3b82f6")
                        .to_string(),
                ),
                cached_expr: RwSignal::new(String::new()),
                cached_ast: RwSignal::new(r.get("query").cloned().unwrap_or(Value::Null)),
            })
            .collect();
        next_rule_id.set(rows.len() as u32);
        config_rules.set(rows);
        show_config_dialog.set(true);
        let ws_id = data
            .get()
            .and_then(|r| r.ok())
            .map(|(w, _, _)| w.id.clone());
        let ids: Vec<u32> = config_rules.get_untracked().iter().map(|r| r.id).collect();
        if ids.is_empty() {
            return;
        }
        let Some(ws_id) = ws_id else { return };
        rules_loading.set(true);
        spawn_local(async move {
            for id in ids {
                let ast = config_rules
                    .get_untracked()
                    .iter()
                    .find(|r| r.id == id)
                    .map(|r| r.cached_ast.get_untracked())
                    .unwrap_or(Value::Null);
                if ast.is_null() {
                    continue;
                }
                if let Ok(s) = format_view_query(&ws_id, &ast).await {
                    config_rules.update(|rows| {
                        if let Some(r) = rows.iter_mut().find(|r| r.id == id) {
                            if r.expr.get_untracked().is_empty() {
                                r.expr.set(s.clone());
                                r.cached_expr.set(s);
                            }
                        }
                    });
                }
            }
            rules_loading.set(false);
        });
    });

    Effect::new_sync(move |_| {
        let s = slug().to_string();
        let _ = refresh.get();
        let _ = refresh_view.get();
        let _ = query_ast.get();
        // 列表整体重载时清空勾选：选中的行可能已不在当前视图/分页里。
        batch_selected.set(Vec::new());
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        // 令牌已被服务端判死时同样回登录页：`logged_out()` 只看本地有没有令牌，
        // 死令牌在启动期被清掉之前它是看不出来的。
        if logged_out() || auth.session_lost.get() {
            navigate("/login", Default::default());
            return;
        }
        // 查询须在 spawn 前同步组装，signal 读取才会登记为 Effect 依赖。
        // 条件来自 query_ast（筛选芯片 / 表达式编辑它），再并入顶栏 ad-hoc 全文词。
        // R16：ad-hoc 词仅在回车时写入 query_ast，故这里用 get_untracked 读取，
        // 避免每次击键都触发重查；query_ast 才是真正的重查触发器。
        let ast = with_text(&query_ast.get(), &ad_hoc_text.get_untracked());
        let sorts = active_view.get().map(|v| v.sorts).unwrap_or_default();
        let page_now = page_signal.get();
        let all = tl_active.get();
        let my_seq = req_seq.get_untracked() + 1;
        req_seq.set(my_seq);
        // URL 上的 `?view=<id>` 快照：本次 Effect 跑出的 load_views 用它作为
        // active_id 种子。`initial_view_id` 是函数级 let，捕获在 Effect 闭包里；
        // 这里再克隆一份交给内部 spawn 的 future，免得 future 直接拿走 Effect
        // 环境里的 Option<String>（不是 Copy，搬一次就没了）。
        let initial_for_load = initial_view_id.clone();
        // 地址里的工作空间可能已不存在（库被重置、链接失效），此时要把用户送回列表。
        let nav_missing = navigate.clone();
        spawn_local(async move {
            let fetched = async {
                let ws = workspace_by_slug(&s).await?;
                let Some(ws) = ws else {
                    return Ok(None);
                };
                // 时间轴要按全量画，两条分支返回同一个 `EntryPage` 形状，
                // 后面写入 `data` 的代码完全共用。
                let ep = if all {
                    query_all_entries(&ws.id, &ast, &sorts).await?
                } else {
                    query_entries(&ws.id, &ast, &sorts, page_now, page_size, None, None).await?
                };
                let schema_list = label_schemas(&ws.id).await?;
                // 成员表只在 Account 型标签或账号列出现时才用得上，但那是加载后的才知道的
                // 信息，多一次请求换掉「打开详情才发现选不了人」的空窗。
                let member_list = members(&ws.id).await.unwrap_or_default();
                Ok::<_, String>(Some((
                    ws,
                    ep.items,
                    schema_list,
                    ep.total,
                    ep.label_names,
                    member_list,
                )))
            }
            .await;
            // 只接受最新一次请求的结果，丢弃乱序返回的旧响应（翻页/排序并发时可能发生）。
            if req_seq.get_untracked() != my_seq {
                return;
            }
            match fetched {
                // 工作空间不存在：整页留着只会让每个操作都报「尚未加载完成」，回列表重选。
                Ok(None) => nav_missing("/workspaces", Default::default()),
                Ok(Some((w, items, list, total, names, member_list))) => {
                    ws_name.set(w.name.clone());
                    schemas.set(list.clone());
                    ws_members.set(member_list);
                    total_signal.set(total);
                    view_label_names.set(names);
                    load_views(w.id.clone(), initial_for_load.clone());
                    data.set(Some(Ok((w, items, list))));
                }
                Err(e) => data.set(Some(Err(e))),
            }
        });
    });

    // 双击行 / AI 生成 Entry 后进入条目全屏页，与详情面板的「全屏」按钮走同一条路由。
    // 当前激活的视图（非默认）以 `?view=<id>` 形式写入 URL：详情页返回时能
    // 精确回到同一视图，而不是落到默认视图。默认视图不必写，纯 URL 已能表达。
    // 手动新建 Entry 不再走这里：只把新行在表格里选中，避免一刀切跳走用户的上下文。
    // 必须先于 `apply_ai`（约 1206 行）与表格行的 `on_open=open_entry` 定义。
    let nav_entry = use_navigate();
    let open_entry = Callback::new(move |code: String| {
        if code.is_empty() {
            return;
        }
        let url = match active_id.get_untracked() {
            Some(id) if !id.is_empty() => {
                let is_default = active_view
                    .get_untracked()
                    .is_some_and(|v| v.is_default);
                if is_default {
                    format!("/{}/entry/{}", slug(), code)
                } else {
                    format!("/{}/entry/{}?view={}", slug(), code, id)
                }
            }
            _ => format!("/{}/entry/{}", slug(), code),
        };
        nav_entry(&url, Default::default());
    });

    let create_submit = move |ev: SubmitEvent| {
        ev.prevent_default();
        let t = new_title.get();
        // 表单里已填的标签值先取快照；空输入不进列表。
        let planned: Vec<(String, Value)> = new_labels
            .get_untracked()
            .iter()
            .filter_map(|r| r.to_value().map(|v| (r.name.clone(), v)))
            .collect();
        let Some(id) = data
            .get()
            .and_then(|r| r.ok())
            .map(|(ws, _, _)| ws.id.clone())
        else {
            return;
        };
        spawn_local(async move {
            match create_entry(&id, &t, None).await {
                Err(e) => error.set(Some(e)),
                Ok(created) => {
                    // 服务端 createEntry 只接收标题，标签需在条目存在后逐个 upsert。
                    // 单个标签失败不丢弃已创建的条目，但要如实报出来。
                    for (name, value) in &planned {
                        if let Err(e) = set_labeling(&created.code, name, value).await {
                            error.set(Some(i18n.tf("entry.errors.label_write_failed", &[("name", &name), ("err", &e.to_string())])));
                            break;
                        }
                    }
                    new_title.set(String::new());
                    new_labels.set(Vec::new());
                    show_new.set(false);
                    refresh.update(|n| *n += 1);
                    // 新建后在表格里选中该行：分页位置不可预测时，由「选中 N 项」徽标
                    // 显式表达选中状态，不强行进入全屏页打断用户的上下文。AI 生成
                    // 路径仍走 open_entry.run（见 apply_ai）。同时打开右侧详情——
                    // 这是「点标题」的等价路径，创建完应该让用户立刻看到内容。
                    selected.set(created.code.clone());
                    panel_code.set(created.code);
                }
            }
        });
    };

    // 「新建 Entry」表单打开（show_new 由 false 变 true）那一刻把焦点交给标题输入框。
    // Effect 只在表单渲染完成后才有可用的 NodeRef——以 false → true 的边沿为信号，
    // 而不是依赖 show_new 的当前值；避免「表单已经渲染了再打开页面」这种首挂载态抢焦点。
    #[cfg(target_arch = "wasm32")]
    Effect::new(move |prev: Option<bool>| {
        let now = show_new.get();
        if prev == Some(false) && now {
            if let Some(el) = new_title_ref.get() {
                let _ = el.focus();
            }
        }
        now
    });

    // 上下键在 Entry 表格里切换选中条目。
    // 跳过规则：光标在输入框 / 富文本 / 下拉里时不抢，按 Enter 走的就是这些控件的语义。
    // 越界处理：到页首向上再翻上一页（停在末位）；到页末向下再翻下一页（停在首位）。
    #[cfg(target_arch = "wasm32")]
    {
        let handle = window_event_listener(leptos::ev::keydown, move |ev| {
            // 表格没渲染或未加载完成时也不抢：避免选中一个不存在的条目。
            let Some(items) = data.get().and_then(|r| r.ok()).map(|(_, it, _)| it) else {
                return;
            };
            if items.is_empty() {
                return;
            }
            // 焦点在文本输入控件时不响应：用户可能在改表达式 / 全文检索。
            let target = ev.target();
            let is_text_input = target
                .as_ref()
                .and_then(|t| {
                    t.dyn_ref::<leptos::web_sys::HtmlInputElement>().map(|el| {
                        let t = el.type_();
                        t == "text"
                            || t == "search"
                            || t == "email"
                            || t == "password"
                            || t == "url"
                    })
                })
                .unwrap_or(false);
            let is_textarea = target
                .as_ref()
                .and_then(|t| {
                    t.dyn_ref::<leptos::web_sys::HtmlTextAreaElement>()
                        .map(|_| true)
                })
                .unwrap_or(false);
            let is_select = target
                .as_ref()
                .and_then(|t| {
                    t.dyn_ref::<leptos::web_sys::HtmlSelectElement>()
                        .map(|_| true)
                })
                .unwrap_or(false);
            let is_ce = target
                .as_ref()
                .and_then(|t| {
                    t.dyn_ref::<leptos::web_sys::HtmlElement>()
                        .map(|el| el.is_content_editable())
                })
                .unwrap_or(false);
            if is_text_input || is_textarea || is_select || is_ce {
                return;
            }
            let key = ev.key();
            if key != "ArrowDown" && key != "ArrowUp" {
                return;
            }
            ev.prevent_default();
            let codes: Vec<String> = items.iter().map(|e| e.code.clone()).collect();
            let cur = selected.get_untracked();
            let idx = codes.iter().position(|c| c == &cur);
            let new_idx = match (idx, key.as_str()) {
                (Some(i), "ArrowDown") => {
                    if i + 1 < codes.len() {
                        i + 1
                    } else {
                        // 已经在末位：尝试翻到下一页首位。
                        let total = total_signal.get_untracked();
                        let pages = ((total + page_size - 1) / page_size).max(1);
                        let cur_page = page_signal.get_untracked();
                        if cur_page < pages {
                            page_signal.set(cur_page + 1);
                            0
                        } else {
                            i
                        }
                    }
                }
                (Some(i), "ArrowUp") => {
                    if i > 0 {
                        i - 1
                    } else {
                        let cur_page = page_signal.get_untracked();
                        if cur_page > 1 {
                            page_signal.set(cur_page - 1);
                            // 翻页前 items 仍是旧页数据，等新页加载后会重新触发该判断。
                            // 这里直接选 0 也无妨：未翻页前就是 0。
                            0
                        } else {
                            i
                        }
                    }
                }
                // 表格里没有任何选中条目：从首 / 末位开始，给键盘一个落脚点。
                (None, "ArrowDown") => 0,
                (None, "ArrowUp") => codes.len() - 1,
                _ => return,
            };
            if let Some(code) = codes.get(new_idx) {
                // 键盘上下移动：行高亮永远跟随新行；详情面板按当前状态决定——
                // 若 panel_code 当前已打开（同一行），就跟随到新行；
                // 若用户已关掉（panel_code 为空），保持关闭。
                // 这样用户在导航开始时面板的开关态是 sticky 的，按 X 中途关掉
                // 之后下一次 ↓ 也不会再把它撬开。
                // 与点击标题后的解耦语义一致：selected 与 panel_code 不再绑死。
                selected.set(code.clone());
                if !panel_code.get_untracked().is_empty() {
                    panel_code.set(code.clone());
                }
            }
        });
        on_cleanup(move || handle.remove());
    }

    // 打开批量弹窗：每次按当前标签 schema 重建草稿，抵消上一次的残留。
    let open_batch = move |_| {
        batch_labels.set(
            schemas
                .get_untracked()
                .into_iter()
                .map(DraftLabel::from_schema)
                .collect(),
        );
        batch_error.set(None);
        show_batch.set(true);
    };

    let apply_batch = move |_| {
        let codes = batch_selected.get_untracked();
        // 空输入一律视为「本次不写这个标签」，与新建表单的语义一致。
        let pairs: Vec<Value> = batch_labels
            .get_untracked()
            .iter()
            .filter_map(|r| {
                r.to_value()
                    .map(|v| serde_json::json!({ "name": r.name, "value": v }))
            })
            .collect();
        if pairs.is_empty() {
            batch_error.set(Some(i18n.t("entry.errors.label_required")));
            return;
        }
        batch_busy.set(true);
        batch_error.set(None);
        spawn_local(async move {
            // 整批原子写入：服务端任一取值非法则整批失败，界面上不会出现「写了一半」。
            match set_labelings(&codes, &Value::Array(pairs)).await {
                Ok(_) => {
                    batch_busy.set(false);
                    show_batch.set(false);
                    batch_labels.set(Vec::new());
                    batch_selected.set(Vec::new());
                    refresh.update(|n| *n += 1);
                }
                Err(e) => {
                    batch_busy.set(false);
                    batch_error.set(Some(e));
                }
            }
        });
    };

    let open_ai = move |_| {
        show_ai.set(true);
        ai_error.set(None);
        ai_scenario.set(String::new());
        ai_tone.set(String::new());
        let Some(ws_id) = data
            .get_untracked()
            .and_then(|r| r.ok())
            .map(|(w, _, _)| w.id)
        else {
            return;
        };
        ai_loading.set(true);
        spawn_local(async move {
            match workspace_ai_config(&ws_id).await {
                Ok(cfg) => {
                    ai_scenarios.set(cfg.scenarios);
                    ai_tones.set(cfg.tones);
                }
                Err(e) => ai_error.set(Some(e)),
            }
            ai_loading.set(false);
        });
    };

    let apply_ai = move |_| {
        let codes = batch_selected.get_untracked();
        if codes.is_empty() {
            return;
        }
        let Some(ws_id) = data
            .get_untracked()
            .and_then(|r| r.ok())
            .map(|(w, _, _)| w.id)
        else {
            return;
        };
        let scenario = ai_scenario.get_untracked();
        let tone = ai_tone.get_untracked();
        ai_busy.set(true);
        ai_error.set(None);
        spawn_local(async move {
            let result = summarize_entries(
                &ws_id,
                &codes,
                (!scenario.is_empty()).then_some(scenario.as_str()),
                (!tone.is_empty()).then_some(tone.as_str()),
            )
            .await;
            match result {
                Ok(created) => {
                    ai_busy.set(false);
                    show_ai.set(false);
                    batch_selected.set(Vec::new());
                    refresh.update(|n| *n += 1);
                    // 生成的是新条目：直接进全屏页，用户不必再去列表里翻。
                    // 用 created.code 而不是等列表重查后再定位——列表分页位置不可预测。
                    open_entry.run(created.code);
                }
                Err(e) => {
                    ai_busy.set(false);
                    ai_error.set(Some(e));
                }
            }
        });
    };

    // 拉取已归档条目列表（弹窗打开时 + 归档/取消归档后刷新）。
    let load_archived = move || {
        let Some(ws_id) = data
            .get()
            .and_then(|r| r.ok())
            .map(|(w, _, _)| w.id.clone())
        else {
            return;
        };
        archived_busy.set(true);
        spawn_local(async move {
            match archived_entries(&ws_id).await {
                Ok(list) => {
                    archived_list.set(list);
                    archived_error.set(None);
                }
                Err(e) => archived_error.set(Some(e)),
            }
            archived_busy.set(false);
        });
    };

    let open_archived = move |_| {
        archived_open.set(true);
        load_archived();
    };

    // 取消归档单条：成功后就地从列表移除，避免整表重拉。
    let restore_archived = move |c: String| {
        spawn_local(async move {
            match unarchive_entry(&c).await {
                Ok(_) => {
                    archived_list.update(|l| l.retain(|e| e.code != c));
                    archived_error.set(None);
                    refresh.update(|n| *n += 1);
                }
                Err(e) => archived_error.set(Some(e)),
            }
        });
    };

    // 批量归档勾选的条目。服务端只有单条 mutation，故逐条请求：中途失败如实报告
    // 已成功的条数，不假装整批成功，也不回滚已归档的条目（归档本身可逆）。
    let archive_selected = move |_| {
        let codes = batch_selected.get_untracked();
        if codes.is_empty() {
            return;
        }
        batch_busy.set(true);
        spawn_local(async move {
            let mut done = 0usize;
            let mut failure = None;
            for c in &codes {
                match archive_entry(c).await {
                    Ok(true) => done += 1,
                    Ok(false) => {
                        failure = Some(i18n.t("entry.errors.archive_failed"));
                        break;
                    }
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            batch_busy.set(false);
            // 已归档的条目已不在当前列表里，清空勾选避免对不存在的行再操作。
            batch_selected.set(Vec::new());
            refresh.update(|n| *n += 1);
            if let Some(e) = failure {
                error.set(Some(i18n.tf("entry.errors.archive_partial", &[("done", &done.to_string()), ("err", &e.to_string())])));
            }
        });
    };

    // 「新建」按钮的两个回调：原先由侧栏传入，现在顶栏 AppBar 通过共享槽
    // 读走——其它页面不在 WorkspaceMain 这个 slot 里就不会渲染这个按钮。
    // 提到 `view!` 之外是为了既能写槽、也能本地复用。
    let on_new_entry = Callback::new(move |_| {
        // 打开时按当前标签 schema 重建待填行（新建与取消都重置）。
        // 视图列与视图表达式里用到的标签预填默认值：这两类标签
        // 是当前视图关心的，新条目本来就该带着它们。
        if !show_new.get_untracked() {
            let auto = auto_label_names(
                active_view
                    .get_untracked()
                    .map(|v| v.columns)
                    .unwrap_or_default(),
                &query_ast.get_untracked(),
            );
            new_labels.set(
                schemas
                    .get_untracked()
                    .into_iter()
                    .map(|s| {
                        if auto.iter().any(|n| n == &s.name) {
                            DraftLabel::from_schema_preset(s)
                        } else {
                            DraftLabel::from_schema(s)
                        }
                    })
                    .collect(),
            );
        }
        show_new.set(!show_new.get_untracked());
    });
    let on_new_view = Callback::new(move |_| {
        batch(move || {
            view_dialog_saveas.set(false);
            view_query_input.set(serde_json::json!({ "and": [] }));
            view_sort_input.set(Vec::new());
            view_name_input.set(String::new());
            // 「新建视图」默认勾选更新时间——与「配置视图」弹窗的行为对齐。
            view_columns_input.set(vec![COLUMN_UPDATED_AT.to_string()]);
            view_shared_input.set(false);
            dialog_error.set(None);
            show_view_dialog.set(true);
        });
    });
    if let Some(slot) = crate::frontend::use_context::<crate::frontend::WorkspaceNewMenuSlot>() {
        slot.current.set(Some(crate::frontend::WorkspaceNewMenu {
            on_new_entry,
            on_new_view,
        }));
        // 离开 WorkspaceMain（切到 /workspaces、登录页等）时清掉槽，
        // 否则切走之后 AppBar 还会显示按钮——回调闭包里有本页信号，
        // 留着它指向死数据也没意义。
        on_cleanup(move || slot.current.set(None));
    }
    // 「时间轴切换」按钮：仅当前激活视图配了时间轴时才往 AppBar 推——普通视图
    // 强行塞个切换按钮毫无意义。`timeline_mode` 已经是本页的状态源，直接
    // 把信号搬过去当视觉态驱动即可。
    if let Some(slot) = crate::frontend::use_context::<crate::frontend::WorkspaceTimelineSlot>() {
        let on_toggle = Callback::new(move |_| timeline_mode.set(!timeline_mode.get()));
        slot.current
            .set(Some(crate::frontend::WorkspaceTimelineToggle {
                on_toggle,
                is_timeline_mode: timeline_mode,
            }));
        on_cleanup(move || slot.current.set(None));
    }

    view! {
        <div class="page page-app">
            // Agent 副作用落地时的非阻塞提示：view_query_replace parse 失败等
            // 场景会让用户以为 agent 罢工,这里挂 5s 自消失的横幅。
            <Show
                when=move || agent_warning.get().is_some()
                fallback=move || view! { <span></span> }
            >
                <div class="agent-warn" role="status">
                    {move || agent_warning.get().unwrap_or_default()}
                </div>
            </Show>
            <div class="crumb" style="display:flex;align-items:center;gap:8px">
                <A href="/workspaces">
                    <button class="btn sm">{ic_back()}{i18n.t("entry.back_workspaces_btn")}</button>
                </A>
                <span>{move || match active_view.get() {
                    Some(v) => format!("/{} · {}", slug(), i18n.tf("entry.view_label", &[("name", &v.name)])),
                    None => format!("/{} · {}", slug(), i18n.t("sidebar.base_view_all_entries")),
                }}</span>
            </div>
            <div class=move || if sidebar_collapsed.get() { "ws-layout collapsed" } else { "ws-layout" }>
                <WorkspaceSidebar
                    slug=slug().to_string()
                    name=ws_name
                    views=view_list
                    active=active_id
                    collapsed=sidebar_collapsed
                    on_select=select_view
                    on_archived=Callback::new(open_archived)
                    on_context=on_context_view
                />
                <div class="panel wmain">
                    <div class="filters">
                        <div class="querybar">
                            <div class="q-expr">
                            <div class="q-expr-tools">
                                <button class="exprhelp" title=i18n.t("entry.expr_help")
                                    on:click=move |_| expr_help.set(true)>{ic_help()}</button>
                                // 重置按钮：把表达式与 ad-hoc 全文词还原到「视图已落库的基线」。
                                // 基础视图的基线恒为空；普通视图的基线是上次保存的 query_expr。
                                // `view_dirty` 同时看 AST 与两个输入框里的未应用文本，
                                // 确保敲完未回车也能一键还原；与基线一致时按钮置灰。
                                {move || if active_view.get().is_some() {
                                    view! {
                                        <button class="exprreset" title=i18n.t("entry.expr_reset")
                                            disabled=move || !view_dirty()
                                            on:click=move |_| {
                                                query_ast.set(saved_baseline.get_untracked());
                                                ad_hoc_text.set(String::new());
                                                expr_text.set(saved_baseline_expr.get_untracked());
                                                hint_open.set(false);
                                                time_pick.set(None);
                                                refresh_view.update(|n| *n += 1);
                                            }>{ic_undo()}</button>
                                    }.into_any()
                                } else {
                                    ().into_any()
                                }}
                            </div>
                            <input class="inp" style="flex:1;min-width:0"
                                placeholder=i18n.t("entry.expr_placeholder")
                                prop:value=expr_text
                                on:input=move |ev| {
                                    let v = event_target_value(&ev);
                                    hint_open.set(label_fragment(&v).is_some());
                                    // 时间型键（date/time/datetime 标签或 CreatedAt /
                                    // UpdatedAt）后接比较运算符再一个空白，就浮出原生控件。
                                    // 键与运算符之间可有空白。
                                    let kind_of = |key: &str| -> Option<TimeKind> {
                                        if key.eq_ignore_ascii_case("CreatedAt")
                                            || key.eq_ignore_ascii_case("UpdatedAt")
                                        {
                                            return Some(TimeKind::DateTime);
                                        }
                                        schemas
                                            .get_untracked()
                                            .iter()
                                            .find(|s| s.name.eq_ignore_ascii_case(key))
                                            .and_then(|s| match s.value_type.as_str() {
                                                "date" => Some(TimeKind::Date),
                                                "time" => Some(TimeKind::Time),
                                                "datetime" => Some(TimeKind::DateTime),
                                                _ => None,
                                            })
                                    };
                                    time_pick.set(detect_time_picker(&v, &kind_of));
                                    expr_text.set(v);
                                }
                                on:keydown=move |ev| {
                                    if ev.key() == "Enter" {
                                        ev.prevent_default();
                                        hint_open.set(false);
                                        time_pick.set(None);
                                        apply_expr();
                                    } else if ev.key() == "Escape" {
                                        hint_open.set(false);
                                        time_pick.set(None);
                                    }
                                }
                                on:blur=move |_| hint_open.set(false) />
                            {move || {
                                if !hint_open.get() {
                                    return ().into_any();
                                }
                                let cur = expr_text.get();
                                let Some(frag) = label_fragment(&cur) else {
                                    return ().into_any();
                                };
                                let frag = frag.to_lowercase();
                                // 候选按标签的展示名（title，如「任务」）呈现——用户认的是它；
                                // 插进表达式的仍是标签 key（如 Task）。key 以淡色跟在后面，说明落进输入框的是什么。
                                let schemas_now = schemas.get();
                                // 先摆 7 个内置元数据名（用户不用先给视图打上这些标签才能引用），
                                // 再摆本视图已有标签；同名（大小写无关，老 schema 可能留着小写 code）只留内置。
                                let mut items: Vec<(String, String)> = builtin_fields(&i18n)
                                    .into_iter()
                                    .map(|(k, t)| (k, t))
                                    .collect();
                                for name in view_label_names.get() {
                                    if items.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) {
                                        continue;
                                    }
                                    let title = schemas_now
                                        .iter()
                                        .find(|s| s.name == name)
                                        .map(|s| s.title.trim().to_string())
                                        .filter(|t| !t.is_empty())
                                        .unwrap_or_else(|| name.clone());
                                    items.push((name, title));
                                }
                                let items: Vec<(String, String)> = items
                                    .into_iter()
                                    .filter(|(name, title)| {
                                        name.to_lowercase().contains(&frag)
                                            || title.to_lowercase().contains(&frag)
                                    })
                                    .collect();
                                if items.is_empty() {
                                    return ().into_any();
                                }
                                view! {
                                    <div class="lblhint">
                                        {items.into_iter().map(|(name, title)| {
                                            let n = name.clone();
                                            view! {
                                                <div class="lblhint-it"
                                                    on:mousedown=move |ev| { ev.prevent_default(); pick_label(n.clone()); }>
                                                    <span>{title}</span>
                                                    <span class="mut">{name}</span>
                                                </div>
                                            }
                                        }).collect::<Vec<_>>()}
                                    </div>
                                }.into_any()
                            }}
                            {move || time_pick.get().map(|(kind, at)| {
                                // 控件值 → 存储串（补秒、datetime 去 T 换空格）交给共享实现，
                                // 浏览器多吐的秒不会被二次拼接。
                                let vt = match kind {
                                    TimeKind::Date => "date",
                                    TimeKind::Time => "time",
                                    TimeKind::DateTime => "datetime",
                                };
                                let input_type = match kind {
                                    TimeKind::Date => "date",
                                    TimeKind::Time => "time",
                                    TimeKind::DateTime => "datetime-local",
                                };
                                view! {
                                    <div class="timepick">
                                        // 原生 datetime-local 控件在挂载时锁住 <html lang>；
                                        // Leptos 0.8 不支持给原始 HTML 元素挂 key 触发
                                        // 重建（HtmlElement::key 不存在），切语言后需要
                                        // 一次页面刷新让日历界面与新 lang 对齐。语言文本
                                        // （标签 / 提示）的切换仍由 move || i18n.t() 兜住。
                                        <input type=input_type on:change=move |ev| {
                                            let raw = event_target_value(&ev);
                                            let Some(formatted) = from_native(vt, &raw) else { return; };
                                            let cur = expr_text.get_untracked();
                                            // 时间值必须加引号，否则词法器会把它当数字解析。
                                            let head = cur.get(..at).unwrap_or(&cur).to_string();
                                            expr_text.set(format!("{head}\"{formatted}\" "));
                                            time_pick.set(None);
                                            hint_open.set(false);
                                        } />
                                    </div>
                                }
                            })}
                            </div>
                            <label class="q-search" title=i18n.t("entry.search_view")>
                                {ic_search()}
                                <input placeholder=i18n.t("entry.search_view") prop:value=ad_hoc_text
                                    on:input=move |ev| ad_hoc_text.set(event_target_value(&ev))
                                    on:keydown=move |ev| {
                                        if ev.key() == "Enter" {
                                            let mut ast = query_ast.get();
                                            ast = with_text(&ast, &ad_hoc_text.get());
                                            query_ast.set(ast);
                                            page_signal.set(1);
                                            refresh_view.update(|n| *n += 1);
                                        }
                                    } />
                            </label>
                        </div>
                        {move || if active_view.get().is_some_and(|v| v.is_default) {
                            // 基础视图上的过滤是临时态：不给「保存」，要留存只能另存为新视图。
                            // 仅在表达式 / 全文搜索有未落库改动时才点亮，否则按钮无意义。
                            if !view_dirty() {
                                ().into_any()
                            } else {
                                view! {
                                    <button class="ibtn" style="margin-left:auto" title=i18n.t("entry.saveas_view")
                                        on:click=move |_| {
                                            let Some(v) = active_view.get() else { return };
                                            let ast = query_ast.get_untracked();
                                            let sorts = v.sorts.clone();
                                            let cols = v.columns.clone();
                                            dialog_error.set(None);
                                            batch(move || {
                                                view_dialog_saveas.set(true);
                                                view_query_input.set(ast);
                                                view_sort_input.set(sorts);
                                                view_name_input.set(String::new());
                                                view_columns_input.set(cols);
                                                view_shared_input.set(false);
                                                show_view_dialog.set(true);
                                            });
                                        }>{ic_copy()}</button>
                                }.into_any()
                            }
                        } else {
                            view! {
                                <button class="ibtn" style="margin-left:auto" disabled=move || !view_savable()
                                    title=i18n.t("entry.save_view")
                                    on:click=move |_| {
                                        let Some(v) = active_view.get() else { return };
                                        let ast = query_ast.get();
                                        let sorts = v.sorts.clone();
                                        let cols = v.columns.clone();
                                        let shared = v.is_shared;
                                        let id = v.id.clone();
                                        let name = v.name.clone();
                                        let title_colors = v.title_colors.clone();
                                        // 保存后用服务端权威的 query_expr 同步基线文本——比
                                        // 直接拿 expr_text 更稳，规避两端规范化差异导致重置按钮残留亮起。
                                        let saved_expr_text = expr_text.get_untracked();
                                        spawn_local(async move {
                                            match update_view(&id, &name, &ast, &sorts, &cols, shared, &title_colors).await {
                                                Ok(saved) => {
                                                    let new_expr_text = saved.query_expr.clone();
                                                    view_list.update(|l| {
                                                        if let Some(slot) = l.iter_mut().find(|x| x.id == saved.id) {
                                                            *slot = saved.clone();
                                                        }
                                                    });
                                                    active_view.set(Some(saved));
                                                    saved_baseline.set(ast);
                                                    saved_baseline_expr.set(new_expr_text);
                                                    // 保存后输入框文本也应与服务端一致，否则 expr_text
                                                    // 与基线分歧会让「重置」按钮误亮。
                                                    if expr_text.get_untracked() != saved_expr_text {
                                                        expr_text.set(saved_expr_text);
                                                    }
                                                    error.set(None);
                                                }
                                                Err(e) => error.set(Some(e)),
                                            }
                                        });
                                    }>{ic_save()}</button>
                            }.into_any()
                        }}
                    </div>

                    {move || if show_new.get() {
                        view! {
                            <form class="filters" on:submit=create_submit>
                                <input class="inp" style="flex:1" placeholder=i18n.t("entry.new_entry_title_ph") node_ref=new_title_ref
                                    prop:value=new_title on:input=move |ev| new_title.set(event_target_value(&ev)) />
                                <button class="btn pri" type="submit">{i18n.t("common.create")}</button>
                                <button class="btn" type="button" on:click=move |_| {
                                    new_labels.set(Vec::new());
                                    show_new.set(false);
                                }>{i18n.t("common.cancel")}</button>
                                <LabelDraft rows=new_labels members=ws_members />
                            </form>
                        }.into_any()
                    } else {
                        view! { <div></div> }.into_any()
                    }}

                    {move || error.get().map(|e| view! { <p class="error" style="padding:8px 16px">{e}</p> })}

                    <div class=move || {
                        // 是否腾出右侧面板的宽度取决于 `panel_code`，而不是 `selected`：
                        // 只点亮高亮但没打开面板时，表格继续占满整行。
                        if panel_code.get().is_empty() {
                            "view-body full".to_string()
                        } else {
                            "view-body".to_string()
                        }
                    }>
                        <div>
                            {move || {
                                // 批量按钮栏只在持久多选模式下显示。
                                // 多选模式由右键行序号开启；ESC / 点表格外区域退出，
                                // 退出后即便 batch_selected 还有残留也不显示按钮——
                                // 没有入口就藏起来，避免「按钮在但点了什么都不发生」的怪态。
                                // 用户再右键任意行序号即可重入模式。
                                let n = batch_selected.get().len();
                                let in_mode = multi_select_mode.get();
                                (n > 0 && in_mode).then(|| view! {
                                    <div class="batchbar">
                                        <span class="mut">{i18n.tf("entry.selected_n", &[("n", &n.to_string())])}</span>
                                        <button class="btn pri sm" on:click=open_batch>{i18n.t("entry.batch_set_labels")}</button>
                                        <button class="btn sm" disabled=move || batch_busy.get() || ai_busy.get()
                                            on:click=open_ai>{i18n.t("entry.ai_summary")}</button>
                                        <button class="btn sm" disabled=move || batch_busy.get()
                                            on:click=archive_selected>{i18n.t("entry.archive")}</button>
                                        <button class="btn sm" on:click=move |_| batch_selected.set(Vec::new())>
                                            {i18n.t("entry.clear_selection")}
                                        </button>
                                    </div>
                                })
                            }}
                            {move || if tl_active.get() {
                                view! {
                                    <TimelineView
                                        data
                                        schemas
                                        members=ws_members
                                        config=Signal::derive(move || -> Option<ViewTimeline> {
                                            active_view.get().and_then(|v| v.timeline)
                                        })
                                        selected
                                        panel_code
                                        on_open=open_entry
                                    />
                                }
                                .into_any()
                            } else {
                                view! {
                                    <EntryTable
                                        data
                                        schemas
                                        selected
                                        panel_code
                                        batch_selected
                                        multi_select_mode
                                        on_open=open_entry
                                        members=ws_members
                                        columns=Signal::derive(move || {
                                            active_view.get().map(|v| v.columns).unwrap_or_default()
                                        })
                                        sorts=Signal::derive(move || {
                                            active_view.get().map(|v| v.sorts).unwrap_or_default()
                                        })
                                        title_colors=Signal::derive(move || {
                                            active_view
                                                .get()
                                                .map(|v| v.title_colors)
                                                .unwrap_or(Value::Null)
                                        })
                                        on_sort=Callback::new(move |req: SortRequest| {
                                            let Some(mut v) = active_view.get_untracked() else {
                                                return;
                                            };
                                            // 双击同字段：把它从排序链里剔除。链删空回到默认排序（更新时间 ↓）。
                                            // 「链是空 → 走默认」这件事由 `SortSpec::default` 表达，
                                            // 这里只在确实删空时把 sorts 留作空 Vec（），让下游按默认键落。
                                            if req.remove {
                                                v.sorts.retain(|s| s.field != req.field);
                                            } else {
                                                let pos = v.sorts.iter().position(|s| s.field == req.field);
                                                v.sorts = match (pos, req.additive) {
                                                    // 已在链上：翻转方向，位置不变。
                                                    (Some(i), _) => {
                                                        let mut s = v.sorts;
                                                        s[i].desc = !s[i].desc;
                                                        s
                                                    }
                                                    // Shift + 未在链上：追加为末位降序键。
                                                    (None, true) => {
                                                        let mut s = v.sorts;
                                                        s.push(ViewSort { field: req.field, desc: true });
                                                        s
                                                    }
                                                    // 未按 Shift 且不在链上：整条链替换成这一个键，默认降序。
                                                    (None, false) => vec![ViewSort { field: req.field, desc: true }],
                                                };
                                            }
                                            // 同一批内改写 active_view 与 page_signal，Effect 只跑一次，
                                            // 避免并发两次请求、旧页码的结果乱序覆盖新结果。
                                            batch({
                                                let v = v.clone();
                                                move || {
                                                    active_view.set(Some(v));
                                                    page_signal.set(1);
                                                }
                                            });
                                            persist_sort(v);
                                        })
                                        on_label_changed=Callback::new(move |_: ()| {
                                            // 表格内联编辑写值后：让条目列表重新拉一次，立即拿到落库后的真实值。
                                            // 也让 EntryPanel（依赖同一份刷新节拍）同步刷新其 labels 缓存。
                                            refresh.update(|n| *n += 1);
                                        })
                                    />
                                }
                                .into_any()
                            }}
                            {move || (!tl_active.get()).then(|| view! {
                                <div class="pager">
                                    <button on:click=move |_| page_signal.update(|p| *p = (*p - 1).max(1))>"‹"</button>
                                    {move || {
                                        let pages = ((total_signal.get() + page_size - 1) / page_size).max(1);
                                        (1..=pages.min(9)).map(|p| {
                                            let cur = page_signal.get();
                                            view! {
                                                <button class=if p == cur { "on" } else { "" }
                                                    on:click=move |_| page_signal.set(p)>{p}</button>
                                            }
                                        }).collect::<Vec<_>>()
                                    }}
                                    <button on:click=move |_| {
                                        let pages = ((total_signal.get() + page_size - 1) / page_size).max(1);
                                        page_signal.update(|p| *p = (*p + 1).min(pages));
                                    }>"›"</button>
                                    <span class="mut">{move || i18n.tf("entry.total_n", &[("n", &total_signal.get().to_string())])}</span>
                                </div>
                            })}
                        </div>
                        <EntryPanel code=panel_code slug=slug().to_string() workspace_id=ws_id schemas members=ws_members refresh />
                    </div>
                </div>
            </div>

            {move || show_batch.get().then(|| view! {
                <div class="dmodal" on:click=move |_| show_batch.set(false)>
                    <div class="panel dmbox" style="max-width:560px" on:click=|ev| ev.stop_propagation()>
                        <h3>{i18n.t("entry.batch_set_labels")}</h3>
                        <p class="mut">{move || i18n.tf("entry.batch_hint", &[("n", &batch_selected.get().len().to_string())])}</p>
                        <LabelDraft rows=batch_labels members=ws_members />
                        {move || batch_error.get().map(|e| view! { <p class="error">{e}</p> })}
                        <div style="display:flex;gap:8px;justify-content:flex-end">
                            <button class="btn" on:click=move |_| show_batch.set(false)>{i18n.t("common.cancel")}</button>
                            <button class="btn pri" disabled=move || batch_busy.get() on:click=apply_batch>
                                {i18n.t("entry.batch_apply")}
                            </button>
                        </div>
                    </div>
                </div>
            })}

            {move || show_ai.get().then(|| view! {
                <div class="dmodal" on:click=move |_| show_ai.set(false)>
                    <div class="panel dmbox" style="max-width:520px" on:click=|ev| ev.stop_propagation()>
                        <h3>{i18n.t("entry.ai_summary")}</h3>
                        <p class="mut">{move || i18n.tf("entry.ai_summary_hint", &[("n", &batch_selected.get().len().to_string())])}</p>
                        {move || if ai_loading.get() {
                            view! { <p class="mut">{i18n.t("settings.ai.loading")}</p> }.into_any()
                        } else {
                            let scenarios = ai_scenarios.get();
                            let tones = ai_tones.get();
                            let none_configured = scenarios.is_empty() && tones.is_empty();
                            view! {
                                <div class="stack">
                                    {none_configured.then(|| view! {
                                        <p class="mut">{i18n.t("ai.none_configured")}</p>
                                    })}
                                    <label class="fld">
                                        <span>{i18n.t("entry.scenario")}</span>
                                        <select class="inp" prop:value=move || ai_scenario.get()
                                            on:change=move |ev| ai_scenario.set(event_target_value(&ev))>
                                            <option value="">{i18n.t("common.not_specified")}</option>
                                            {scenarios.into_iter().map(|p| {
                                                let v = p.name.clone();
                                                view! { <option value=v>{p.name}</option> }
                                            }).collect::<Vec<_>>()}
                                        </select>
                                    </label>
                                    <label class="fld">
                                        <span>{i18n.t("entry.tone")}</span>
                                        <select class="inp" prop:value=move || ai_tone.get()
                                            on:change=move |ev| ai_tone.set(event_target_value(&ev))>
                                            <option value="">{i18n.t("common.not_specified")}</option>
                                            {tones.into_iter().map(|p| {
                                                let v = p.name.clone();
                                                view! { <option value=v>{p.name}</option> }
                                            }).collect::<Vec<_>>()}
                                        </select>
                                    </label>
                                </div>
                            }.into_any()
                        }}
                        {move || ai_error.get().map(|e| view! { <p class="error">{e}</p> })}
                        <div style="display:flex;gap:8px;justify-content:flex-end">
                            <button class="btn" on:click=move |_| show_ai.set(false)>{i18n.t("common.cancel")}</button>
                            <button class="btn pri" disabled=move || ai_busy.get() || ai_loading.get()
                                on:click=apply_ai>
                                {move || if ai_busy.get() { i18n.t("entry.ai_busy") } else { i18n.t("entry.ai_generate") }}
                            </button>
                        </div>
                    </div>
                </div>
            })}

            {move || archived_open.get().then(|| view! {
                <div class="dmodal" on:click=move |_| archived_open.set(false)>
                    <div class="panel dmbox" style="width:720px;max-width:92vw" on:click=|ev| ev.stop_propagation()>
                        <h3>{i18n.t("entry.archived_title")}</h3>
                        <p class="mut">{i18n.t("entry.archived_hint")}</p>
                        {move || archived_error.get().map(|e| view! { <p class="error">{e}</p> })}
                        <div class="archlist">
                            {move || if archived_busy.get() {
                                view! { <p class="mut">{i18n.t("common.loading")}</p> }.into_any()
                            } else if archived_list.get().is_empty() {
                                view! { <p class="mut">{i18n.t("entry.archived_empty")}</p> }.into_any()
                            } else {
                                archived_list.get().into_iter().map(|e| {
                                    let c = e.code.clone();
                                    view! {
                                        <div class="archrow">
                                            <span class="code">{e.code.clone()}</span>
                                            <span class="lbl" style="flex:1">{e.title.clone()}</span>
                                            <span class="mut at">
                                                {e.archived_at.as_deref().map(short_time).unwrap_or_default()}
                                            </span>
                                            <button class="btn sm" on:click=move |_| restore_archived(c.clone())>
                                                {i18n.t("entry.unarchive")}
                                            </button>
                                        </div>
                                    }
                                }).collect::<Vec<_>>().into_any()
                            }}
                        </div>
                        <div style="display:flex;justify-content:flex-end">
                            <button class="btn" on:click=move |_| archived_open.set(false)>{i18n.t("common.close")}</button>
                        </div>
                    </div>
                </div>
            })}

            {move || if expr_help.get() {
                view! {
                    <div class="dmodal" on:click=move |_| expr_help.set(false)>
                        <div class="panel dmbox" style="max-width:560px" on:click=|ev| ev.stop_propagation()>
                            <h3>{i18n.t("entry.expr_help_title")}</h3>
                            <div class="exprdoc">
                                <p>{i18n.t("entry.expr_help_intro")}</p>
                                <table class="tbl">
                                    <thead><tr><th style="width:42%">{i18n.t("entry.expr_help_col_syntax")}</th><th>{i18n.t("entry.expr_help_col_meaning")}</th></tr></thead>
                                    <tbody>
                                        <tr><td><code>"Task"</code></td><td>{i18n.t("entry.expr_help_row_task")}</td></tr>
                                        <tr><td><code>"!Task"</code></td><td>{i18n.t("entry.expr_help_row_not_task")}</td></tr>
                                        <tr><td><code>"Bug AND !Task"</code></td><td>{i18n.t("entry.expr_help_row_and")}</td></tr>
                                        <tr><td><code>"Status = \"Open\""</code></td><td>{i18n.t("entry.expr_help_row_eq")}</td></tr>
                                        <tr><td><code>"Status != \"Done\""</code></td><td>{i18n.t("entry.expr_help_row_neq")}</td></tr>
                                        <tr><td><code>"Status IN (\"Open\", \"Done\")"</code></td><td>{i18n.t("entry.expr_help_row_in")}</td></tr>
                                        <tr><td><code>"Priority >= 2"</code></td><td>{i18n.t("entry.expr_help_row_ge")}</td></tr>
                                        <tr><td><code>"Summary ~ \"登录\""</code></td><td>{i18n.t("entry.expr_help_row_contains")}</td></tr>
                                        <tr><td><code>"L5"</code></td><td>{i18n.t("entry.expr_help_row_direct")}</td></tr>
                                        <tr><td><code>"L4+"</code></td><td>{i18n.t("entry.expr_help_row_lplus_eq")}</td></tr>
                                        <tr><td><code>"!L4+"</code></td><td>{i18n.t("entry.expr_help_row_lplus_ne")}</td></tr>
                                        <tr><td><code>"L4+ = \"V1\""</code></td><td>{i18n.t("entry.expr_help_row_lplus_eq")}</td></tr>
                                    </tbody>
                                </table>
                                <p class="mut">{i18n.t("entry.expr_help_inherit_intro")}
                                    {i18n.t("entry.expr_help_inherit_body")}</p>
                                <p class="mut">{i18n.t("entry.expr_help_keyonly_intro")}</p>
                                <p class="mut">{i18n.t("entry.expr_help_meta_intro")}</p>
                                <p class="mut">{i18n.t("entry.expr_help_tip")}</p>
                            </div>
                            <div style="display:flex;justify-content:flex-end">
                                <button class="btn pri" on:click=move |_| expr_help.set(false)>{i18n.t("common.got_it")}</button>
                            </div>
                        </div>
                    </div>
                }.into_any()
            } else { view! { <div></div> }.into_any() }}

            {move || if show_view_dialog.get() {
                let ws_id = data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.id.clone());
                view! {
                    <div class="dmodal">
                        <div class="panel dmbox">
                            <h3>{move || if view_dialog_saveas.get() { i18n.t("entry.saveas_view_title") } else { i18n.t("entry.new_view_title") }}</h3>
                            <input class="inp" placeholder=i18n.t("entry.view_name_ph") prop:value=view_name_input
                                on:input=move |ev| view_name_input.set(event_target_value(&ev)) />
                            // 输入「/」时弹出已有的非叶子节点提示——避免新视图游离到不存在的层级下。
                            {move || {
                                let cur = view_name_input.get();
                                let prefix = cur.rfind('/').map(|i| cur[..i].to_string()).unwrap_or_default();
                                let mut existing: Vec<String> = view_list
                                    .get()
                                    .into_iter()
                                    .map(|v| v.name.clone())
                                    .filter(|n| n != &cur && (n == &prefix || n.starts_with(&format!("{prefix}/"))))
                                    .collect();
                                existing.sort();
                                existing.dedup();
                                // 只列「非叶子」节点：那些名字本身就是另一条视图名的前缀。
                                let groups: Vec<String> = existing
                                    .iter()
                                    .filter(|n| **n == prefix
                                        || existing.iter().any(|other| other != *n && other.starts_with(&format!("{}/", n))))
                                    .cloned()
                                    .collect();
                                if groups.is_empty() {
                                    ().into_any()
                                } else {
                                    view! {
                                        <div class="lblhint" style="margin-top:4px">
                                            <div class="mut" style="font-size:11px;padding:2px 4px">{i18n.t("entry.view_has_non_leaf")}</div>
                                            {groups.iter().map(|g| {
                                                let g_for_label = g.clone();
                                                let g_for_click = g.clone();
                                                view! {
                                                    <div class="lblhint-it"
                                                        on:mousedown=move |ev| ev.prevent_default()
                                                        on:click=move |_| {
                                                            let cur = view_name_input.get();
                                                            let cur_prefix = cur.rfind('/').map(|i| cur[..i].to_string()).unwrap_or_default();
                                                            let new = if cur_prefix.is_empty() {
                                                                format!("{g_for_click}/")
                                                            } else {
                                                                format!("{g_for_click}/{}", cur.trim_start_matches(&format!("{cur_prefix}/")).trim_start_matches('/'))
                                                            };
                                                            view_name_input.set(new);
                                                        }>
                                                        <span>{g_for_label}</span>
                                                    </div>
                                                }
                                            }).collect::<Vec<_>>()}
                                        </div>
                                    }.into_any()
                                }
                            }}
                            {move || view_dialog_saveas.get().then(|| {
                                // 另存为会把基础视图上那份临时过滤一并落库，得让用户看见落的是什么。
                                let e = expr_text.get();
                                view! {
                                    <p class="mut" style="margin:0;font-size:12px">
                                        {if e.trim().is_empty() {
                                            i18n.t("entry.condition_all").to_string()
                                        } else {
                                            i18n.tf("entry.condition_e", &[("expr", &e)])
                                        }}
                                    </p>
                                }
                            })}
                            <div class="dfield">
                                <span class="dlabel">{i18n.t("entry.display_columns")}</span>
                                <ColumnPicker schemas=schemas selected=view_columns_input />
                            </div>
                            <label style="display:flex;gap:6px;align-items:center">
                                <input type="checkbox" prop:checked=view_shared_input
                                    on:change=move |ev| view_shared_input.set(event_target_checked(&ev)) />
                                {i18n.t("entry.share_with_workspace")}
                            </label>
                            {move || dialog_error.get().map(|e| view! { <p class="error" style="margin:0">{e}</p> })}
                            <div style="display:flex;gap:8px;justify-content:flex-end">
                                <button class="btn" on:click=move |_| show_view_dialog.set(false)>{i18n.t("common.cancel")}</button>
                                <button class="btn pri" on:click=move |_| {
                                    let Some(ws_id) = ws_id.clone() else {
                                        dialog_error.set(Some(i18n.t("entry.errors.workspace_not_loaded")));
                                        return;
                                    };
                                    let name = view_name_input.get();
                                    let shared = view_shared_input.get();
                                    let cols = view_columns_input.get();
                                    let query = view_query_input.get_untracked();
                                    let sorts = view_sort_input.get_untracked();
                                    dialog_error.set(None);
                                    spawn_local(async move {
                                        match create_view(&ws_id, &name, &query, &sorts, &cols, shared, &serde_json::json!([])).await {
                                            Ok(v) => {
                                                view_list.update(|l| l.push(v.clone()));
                                                set_active(Some(v));
                                                show_view_dialog.set(false);
                                            }
                                            Err(e) => dialog_error.set(Some(e)),
                                        }
                                    });
                                }>{move || if view_dialog_saveas.get() { i18n.t("entry.saveas_view") } else { i18n.t("common.create") }}</button>
                            </div>
                        </div>
                    </div>
                }.into_any()
            } else { view! { <div></div> }.into_any() }}

            {move || if show_config_dialog.get() {
                view! {
                    <div class="dmodal" on:click=move |_| show_config_dialog.set(false)>
                        <div class="panel dmbox" on:click=|ev| ev.stop_propagation()>
                            <h3>{i18n.t("entry.config_view")}</h3>
                            {move || (!active_view.get().is_some_and(|v| v.is_default)).then(|| view! {
                                // 基础视图的名字是固定概念，不提供改名；服务端也会挡下。
                                <input class="inp" placeholder=i18n.t("entry.view_name_ph") prop:value=config_name_input
                                    on:input=move |ev| config_name_input.set(event_target_value(&ev)) />
                            })}
                            <div class="dfield">
                                <span class="dlabel">{i18n.t("entry.display_columns")}</span>
                                <ColumnPicker schemas=schemas selected=config_columns_input />
                            </div>
                            <div class="dfield">
                                <span class="dlabel">{i18n.t("entry.timeline_view")}</span>
                                <select class="inp" prop:value=config_tl_start
                                    on:change=move |ev| config_tl_start.set(event_target_value(&ev))>
                                    <option value="">{i18n.t("entry.timeline_start_not_set")}</option>
                                    {move || label_options(schemas.get(), true)}
                                </select>
                                <select class="inp" prop:value=config_tl_end
                                    on:change=move |ev| config_tl_end.set(event_target_value(&ev))>
                                    <option value="">{i18n.t("entry.timeline_end_not_set")}</option>
                                    {move || label_options(schemas.get(), true)}
                                </select>
                                <select class="inp" prop:value=config_tl_person
                                    on:change=move |ev| config_tl_person.set(event_target_value(&ev))>
                                    <option value="">{i18n.t("entry.timeline_person_not_set")}</option>
                                    {move || label_options(schemas.get(), false)}
                                </select>
                                <p class="mut" style="font-size:11px;margin:2px 0 0">
                                    {i18n.t("entry.timeline_kind_mismatch")}
                                </p>
                            </div>
                            <label style="display:flex;gap:6px;align-items:center">
                                <input type="checkbox" prop:checked=config_shared_input
                                    on:change=move |ev| config_shared_input.set(event_target_checked(&ev)) />
                                {i18n.t("entry.share_with_workspace")}
                            </label>
                            <div class="tc-rules">
                                <div class="tc-head">
                                    <span>{i18n.t("entry.title_color_rules")}</span>
                                    <button class="btn sm" type="button" on:click=add_rule>{i18n.t("entry.add_rule")}</button>
                                </div>
                                <For
                                    each=move || config_rules.get()
                                    key=|r| r.id
                                    children=move |r: TitleRule| {
                                        let exc = r.expr;
                                        let col = r.color;
                                        let rid = r.id;
                                        // 标题颜色规则的表达式输入也需要 `/` 提示：候选 = 7 个内置元数据 + 本视图列出的标签。
                                        let hint_open = RwSignal::new(false);
                                        let pick_label_tc = {
                                            let exc = exc.clone();
                                            move |name: String| {
                                                let cur = exc.get_untracked();
                                                exc.set(match cur.rfind('/') {
                                                    Some(i) => format!("{}{name} ", &cur[..i]),
                                                    None => format!("{name} "),
                                                });
                                                hint_open.set(false);
                                            }
                                        };
                                        view! {
                                            <div class="tc-row">
                                                <div class="exprwrap" style="flex:1;min-width:0">
                                                    <input class="inp tc-expr" type="text"
                                                        placeholder=i18n.t("entry.condition_placeholder")
                                                        prop:value=move || exc.get()
                                                        on:input=move |ev| {
                                                            let v = event_target_value(&ev);
                                                            hint_open.set(label_fragment(&v).is_some());
                                                            exc.set(v);
                                                        }
                                                        on:blur=move |_| hint_open.set(false) />
                                                    {move || {
                                                        if !hint_open.get() { return ().into_any(); }
                                                        let cur = exc.get();
                                                        let Some(frag) = label_fragment(&cur) else {
                                                            return ().into_any();
                                                        };
                                                        let frag = frag.to_lowercase();
                                                        let schemas_now = schemas.get();
                                                        let mut items: Vec<(String, String)> = builtin_fields(&i18n)
                                                            .into_iter()
                                                            .collect();
                                                        for s in &schemas_now {
                                                            if !items.iter().any(|(n, _)| n.eq_ignore_ascii_case(&s.name)) {
                                                                let t = if s.title.trim().is_empty() { s.name.clone() } else { s.title.clone() };
                                                                items.push((s.name.clone(), t));
                                                            }
                                                        }
                                                        let items: Vec<(String, String)> = items
                                                            .into_iter()
                                                            .filter(|(n, t)| n.to_lowercase().contains(&frag) || t.to_lowercase().contains(&frag))
                                                            .collect();
                                                        if items.is_empty() { return ().into_any(); }
                                                        view! {
                                                            <div class="lblhint">
                                                                {items.into_iter().map(|(name, title)| {
                                                                    let n = name.clone();
                                                                    let pick = pick_label_tc.clone();
                                                                    view! {
                                                                        <div class="lblhint-it"
                                                                            on:mousedown=move |ev| ev.prevent_default()
                                                                            on:click=move |_| pick(n.clone())>
                                                                            <span>{title}</span>
                                                                            <span class="mut">{name}</span>
                                                                        </div>
                                                                    }
                                                                }).collect::<Vec<_>>()}
                                                            </div>
                                                        }.into_any()
                                                    }}
                                                </div>
                                                <ColorPick small=true title=i18n.t("entry.title_color").to_string()
                                                    value=Signal::derive(move || col.get())
                                                    ws_id=ws_id
                                                    on_pick=Callback::new(move |c: String| col.set(c)) />
                                                <button class="vc-op vc-del" type="button" title=i18n.t("common.delete")
                                                    on:click=move |_| config_rules.update(|rows| rows.retain(|x| x.id != rid))>"×"</button>
                                            </div>
                                        }
                                    }
                                />
                                <p class="mut" style="font-size:11px;margin:2px 0 0">
                                    {i18n.t("entry.title_color_match_hint")}
                                </p>
                            </div>
                            {move || dialog_error.get().map(|e| view! { <p class="error" style="margin:0">{e}</p> })}
                            <div style="display:flex;gap:8px;justify-content:flex-end">
                                <button class="btn" on:click=move |_| show_config_dialog.set(false)>{i18n.t("common.cancel")}</button>
                                <button class="btn pri" disabled=move || rules_loading.get() on:click=move |_| {
                                    let Some(v) = active_view.get() else {
                                        dialog_error.set(Some(i18n.t("entry.select_view_first")));
                                        return;
                                    };
                                    dialog_error.set(None);
                                    let id = v.id.clone();
                                    let name = config_name_input.get();
                                    let shared = config_shared_input.get();
                                    let cols = config_columns_input.get();
                                    let ast = query_ast.get();
                                    let sorts = v.sorts.clone();
                                    let tl_start = config_tl_start.get_untracked();
                                    let tl_end = config_tl_end.get_untracked();
                                    let tl_person = config_tl_person.get_untracked();
                                    let ws_id = data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.id.clone());
                                    // 快照规则：条件表达式 + 颜色 + 预填缓存。
                                    let draft: Vec<(String, String, String, Value)> =
                                        config_rules.get_untracked().iter().map(|r| (
                                            r.expr.get_untracked(),
                                            r.color.get_untracked(),
                                            r.cached_expr.get_untracked(),
                                            r.cached_ast.get_untracked(),
                                        )).collect();
                                    spawn_local(async move {
                                        let mut out: Vec<Value> = Vec::new();
                                        for (expr, color, cached_expr, cached_ast) in draft {
                                            let ast = if !expr.trim().is_empty() && expr != cached_expr {
                                                // 条件被编辑过：让服务端解析成 AST；失败则整体不保存。
                                                let Some(ws_id) = ws_id.as_deref() else {
                                                    dialog_error.set(Some(i18n.t("entry.no_workspace_for_query")));
                                                    return;
                                                };
                                                match parse_view_query(ws_id, &expr).await {
                                                    Ok(a) => a,
                                                    Err(e) => {
                                                        dialog_error.set(Some(i18n.tf("entry.invalid_title_color_rule", &[("err", &e.to_string())])));
                                                        return;
                                                    }
                                                }
                                            } else if expr.trim().is_empty() && !cached_expr.is_empty() {
                                                // 预填规则被清空 → 视为删除该条件。
                                                continue;
                                            } else if !cached_ast.is_null() {
                                                // 未编辑（或预填反格式化失败）：直接复用预填 AST。
                                                cached_ast
                                            } else {
                                                // 空条件且无预填 → 未填写，丢弃。
                                                continue;
                                            };
                                            out.push(serde_json::json!({ "query": ast, "color": color }));
                                        }
                                        let title_colors = Value::Array(out);
                                        match update_view(&id, &name, &ast, &sorts, &cols, shared, &title_colors).await {
                                            Ok(saved) => {
                                                // 时间轴配置另发一条 mutation（独立列族、独立并发语义）；
                                                // 失败不回滚视图本身，但要说出来，否则用户以为存上了。
                                                let mut saved = saved;
                                                match set_view_timeline(&saved.id, &tl_start, &tl_end, &tl_person).await {
                                                    Ok(tl) => saved.timeline = tl,
                                                    Err(e) => {
                                                        dialog_error.set(Some(i18n.tf("entry.timeline_save_failed", &[("err", &e.to_string())])));
                                                        return;
                                                    }
                                                }
                                                view_list.update(|l| {
                                                    if let Some(slot) = l.iter_mut().find(|x| x.id == saved.id) {
                                                        *slot = saved.clone();
                                                    }
                                                });
                                                active_view.set(Some(saved));
                                                show_config_dialog.set(false);
                                            }
                                            Err(e) => dialog_error.set(Some(e)),
                                        }
                                    });
                                }>{i18n.t("common.save")}</button>
                            </div>
                        </div>
                    </div>
                }.into_any()
            } else { view! { <div></div> }.into_any() }}

            {move || vmenu.get().map(|(x, y, vid)| {
                let cur = view_list.get_untracked().into_iter()
                    .find(|v| v.id == vid);
                let is_default = cur.as_ref().is_some_and(|v| v.is_default);
                let vid_cfg = vid.clone();
                let vid_del = vid.clone();
                view! {
                    <div class="vmenu"
                        style=format!("left:{x}px;top:{y}px")
                        on:mousedown=move |ev| ev.stop_propagation()
                        on:contextmenu=move |ev| ev.prevent_default()>
                        <button on:click=move |_| {
                            vmenu.set(None);
                            // 复用现有 show_config_dialog 打开逻辑。
                            open_config_for.run(vid_cfg.clone());
                        }>{i18n.t("entry.config_view")}</button>
                        // 删除只对非基础视图开放——基础视图固定存在、不可删。
                        {move || if is_default {
                            ().into_any()
                        } else {
                            let vid_del_btn = vid_del.clone();
                            view! {
                                <button class="danger"
                                    on:click=move |_| {
                                        vmenu.set(None);
                                        delete_view_cb.run(vid_del_btn.clone());
                                    }>{i18n.t("entry.delete_view")}</button>
                            }.into_any()
                        }}
                    </div>
                }.into_any()
            })}
        </div>
    }
}

#[component]
fn WorkspaceSidebar(
    slug: String,
    name: RwSignal<String>,
    views: RwSignal<Vec<View>>,
    active: RwSignal<Option<String>>,
    collapsed: RwSignal<bool>,
    on_select: Callback<String>,
    /// 「已归档」入口：打开归档条目列表弹窗。
    on_archived: Callback<()>,
    /// 右键叶子视图行：参数为 `(view_id, client_x, client_y)`，由父组件负责把
    /// 坐标写入 `vmenu` 让浮层定位。
    on_context: Callback<(String, i32, i32)>,
) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    let list = move || views.get();
    // 基础视图单独置顶展示，不混进「我的 / 共享」两组。
    let default_view = move || list().into_iter().find(|v| v.is_default);
    let mine = move || {
        list()
            .into_iter()
            .filter(|v| !v.is_default && !v.is_shared)
            .collect::<Vec<_>>()
    };
    let shared = move || {
        list()
            .into_iter()
            .filter(|v| !v.is_default && v.is_shared)
            .collect::<Vec<_>>()
    };

    // 把视图列表按 `/` 切分层级。叶子节点的父节点（仅作为路径前缀的视图）保留为
    // 可点击的分组标题，底下挂子视图——这样 `团队/迭代1` 与 `团队/迭代2` 能折叠在一起。
    // `ViewNode` 是视图树上的一个节点：(视图, 深度)。`None` 视图 = 仅作为路径前缀
    // 出现的中间节点（没有对应 View，但需要渲染分组标题）。
    #[derive(Clone)]
    enum ViewNode {
        Leaf(View, usize),
        Group(String, usize),
    }
    fn build_tree(items: Vec<View>) -> Vec<ViewNode> {
        // 收集所有「路径前缀」：每个 view.name 的祖先路径都成为分组节点。
        let mut prefixes = std::collections::BTreeSet::<String>::new();
        let mut leaves: Vec<(View, Vec<String>)> = items
            .into_iter()
            .map(|v| {
                let parts: Vec<String> = v
                    .name
                    .split('/')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                for i in 1..parts.len() {
                    prefixes.insert(parts[..i].join("/"));
                }
                (v, parts)
            })
            .collect();
        // 让分组标题按字典序排在所属叶子之前；分组按路径前缀排序。
        leaves.sort_by(|a, b| a.1.cmp(&b.1));
        let mut out: Vec<ViewNode> = Vec::new();
        // 把每个前缀作为 Group 节点插入到该前缀下第一个叶子节点之前；
        // 同样前缀只插入一次。
        let mut inserted: std::collections::HashSet<String> = Default::default();
        for (v, parts) in leaves {
            for i in 1..parts.len() {
                let prefix = parts[..i].join("/");
                if inserted.insert(prefix.clone()) {
                    out.push(ViewNode::Group(prefix, i.saturating_sub(1)));
                }
            }
            out.push(ViewNode::Leaf(v, parts.len().saturating_sub(1)));
        }
        out
    }

    let row = move |node: ViewNode, shared_mark: bool| {
        match node {
            ViewNode::Leaf(v, depth) => {
                let id = v.id.clone();
                let name = v.name.clone();
                // 叶子视图的展示名只显示最后一段，全名靠 title 提示。
                let leaf_label = name
                    .rsplit('/')
                    .find(|s| !s.trim().is_empty())
                    .unwrap_or(&name)
                    .to_string();
                let count = v.entry_count;
                let is_active = {
                    let id = id.clone();
                    move || active.get().as_deref() == Some(id.as_str())
                };
                let click_id = id.clone();
                let ctx_id = id.clone();
                let indent = format!("padding-left:{}px", 12 + depth * 14);
                view! {
                    <div class=move || {
                             let mut c = String::from("it");
                             if is_active() { c.push_str(" on"); }
                             c
                         }
                         style=indent
                         title=name.clone()
                         on:click=move |_| on_select.run(click_id.clone())
                         on:contextmenu=move |ev: leptos::ev::MouseEvent| {
                             ev.prevent_default();
                             on_context.run((ctx_id.clone(), ev.client_x(), ev.client_y()));
                         }>
                        {if shared_mark {
                            ic_share().into_any()
                        } else {
                            ic_folder().into_any()
                        }}
                        <span class="lbl" style="flex:1">{leaf_label}</span>
                        <span class="n">{count}</span>
                    </div>
                }
                .into_any()
            }
            ViewNode::Group(label, depth) => {
                // 中间路径节点：不挂 view 实体，渲染成不可点的分组标题。
                let indent = format!("padding-left:{}px", 12 + depth * 14);
                view! {
                    <div class="sb-group" style=indent>{label}</div>
                }
                .into_any()
            }
        }
    };

    view! {
        <aside class=move || if collapsed.get() { "panel wside collapsed" } else { "panel wside" }>
            <div class="sb-head">
                <b class="lbl">{move || name.get()}</b>
                <button class="ibtn sb-toggle" title=i18n.t("entry.collapse_sidebar") on:click=move |_| {
                    let v = !collapsed.get_untracked();
                    collapsed.set(v);
                    set_sidebar_collapsed(v);
                }>{move || if collapsed.get() { "»" } else { "«" }}</button>
            </div>
            {move || default_view().map(|v| {
                let id = v.id.clone();
                let count = v.entry_count;
                let is_active = {
                    let id = id.clone();
                    move || active.get().as_deref() == Some(id.as_str())
                };
                let click_id = id.clone();
                let ctx_id = id.clone();
                // 基础视图的名字在服务端是固定字符串（见 service/view.rs：「基础视图
                // 不可改名」），但要在前端按当前 locale 渲染——这里走 i18n。
                let label = move || i18n.t("sidebar.base_view");
                let tooltip = move || i18n.t("sidebar.base_view_tooltip");
                view! {
                    <div class=move || {
                             let mut c = String::from("it base");
                             if is_active() { c.push_str(" on"); }
                             c
                         }
                         title=tooltip
                         on:click=move |_| on_select.run(click_id.clone())
                         on:contextmenu=move |ev: leptos::ev::MouseEvent| {
                             ev.prevent_default();
                             on_context.run((ctx_id.clone(), ev.client_x(), ev.client_y()));
                         }>
                        {ic_tag()}
                        <span class="lbl" style="flex:1">{label}</span>
                        <span class="n">{count}</span>
                    </div>
                }.into_any()
            })}
            <div class="grp">{move || i18n.t("entry.my_views")}</div>
            {move || build_tree(mine()).into_iter().map(|n| row(n, false)).collect::<Vec<_>>()}
            <div class="grp">{move || i18n.t("entry.shared_views")}</div>
            {move || build_tree(shared()).into_iter().map(|n| row(n, true)).collect::<Vec<_>>()}
            <div style="border-top:1px solid var(--line);margin-top:8px;padding-top:8px">
                <div class="it" title=move || i18n.t("entry.archived_title") on:click=move |_| on_archived.run(())>
                    {ic_folder()}<span class="lbl">{move || i18n.t("entry.archived")}</span>
                </div>
                // 设置链接带上当前视图 id：从设置页返回时能精准落到原视图，
                // 而不是默认视图。基础视图不必写，纯 URL 已能表达。
                <A href=move || {
                    let url = format!("/{slug}/settings");
                    let aid = active.get();
                    if aid.as_deref().map_or(false, |id| {
                        !id.is_empty()
                            && list().iter().any(|v| v.id == *id && !v.is_default)
                    }) {
                        format!("{url}?view={}", aid.unwrap())
                    } else {
                        url
                    }
                }>
                    <div class="it" title=move || i18n.t("entry.workspace_settings")>{ic_setting()}<span class="lbl">{move || i18n.t("entry.workspace_settings")}</span></div>
                </A>
            </div>
        </aside>
    }
}

/// 动态列条目表：列来自当前视图（`columns` 里的标签 name）。
/// 标题 / 更新时间 / 展示为列的标签都可点击排序；`Shift` + 点击追加为次级排序键。
#[component]
fn EntryTable(
    data: RwSignal<Option<Result<(Workspace, Vec<Entry>, Vec<LabelSchema>), String>>>,
    schemas: RwSignal<Vec<LabelSchema>>,
    /// 行高亮（`.sel`）：点标题永远置为该行（再点同一行也不取消），
    /// 点其他列走 `<tr>` 的 on:click 翻手/取消——「标题想再点无反应」
    /// 是有意的。
    selected: RwSignal<String>,
    /// 右侧详情面板当前条目：仅由标题点击（或键盘上下移动 / 新建完成）控制；
    /// 点其他列不动它。标题点击会切换该行详情面板的开 / 关——行高亮与
    /// 面板已解耦，允许「行高亮但侧栏空白」（关闭详情时）：点同一行标题
    /// 重新打开面板即可。
    panel_code: RwSignal<String>,
    /// 批量操作勾选的条目 code。与 `selected`（详情面板当前条目）互相独立。
    batch_selected: RwSignal<Vec<String>>,
    /// 持久多选模式：右键行序号开启；模式下点击行只切 batch_selected、不开面板；
    /// ESC 或点表格外区域退出（由外层窗口监听实现，这里只是读写开关）。
    multi_select_mode: RwSignal<bool>,
    /// 双击行时带上条目编码，由外层跳转到条目全屏页。
    on_open: Callback<String>,
    columns: Signal<Vec<String>>,
    /// 当前视图的排序键链，按优先级从高到低。
    sorts: Signal<Vec<ViewSort>>,
    /// 当前视图的标题颜色规则 `[{query,color}]`；命中即给标题上色。
    title_colors: Signal<Value>,
    /// 工作空间成员表：账号型标签列的 id → 姓名。
    members: RwSignal<Vec<Member>>,
    /// 表头点击。`additive` 来自 `Shift` 键。
    on_sort: Callback<SortRequest>,
    /// 表格内联标签编辑成功后通知：让外层重新拉一次条目表，让新值立刻反映在表格。
    on_label_changed: Callback<()>,
) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    let cols = move || columns.get();
    // 当前正在内联编辑的单元格 `(entry_code, label_name)`。None = 全部回到展示态。
    // 同时只允许一个单元格进编辑态——多个编辑态互相打架（焦点 / 落库顺序），没好处。
    // 表头点击触发的排序、或翻页，会让响应式 effect 自动把"还在编辑但已经不在视图里"的
    // 旧 (code, name) 视为已退回——见 cell 闭包内的 `Some((c, n))` 比对。
    let editing_cell: RwSignal<Option<(String, String)>> = RwSignal::new(None);
    // 表格内联编辑的「点别处退出」监听：左键落在编辑态单元格外就退出。
    // 与「右键进入编辑态」共用同一条 editing_cell，但分别监听 mousedown（左）
    // 与 contextmenu（右）。仅挂在 wasm 目标下——SSR 编译时闭包不生成。
    // 用 composed_path 走完整冒泡路径，避免 Chrome 在 `<button>text</button>`
    // 上偶发把 target 设成 Text 节点（旧实现靠 closest 会误判）。composed_path
    // 命中 `.lblcell.editing` 就算「在编辑态里」，连同输入框 / 下拉 / 按钮一起
    // 都不退出；命中其他区域就清掉 editing_cell，toggle_edit 那条右键入口不受
    // 影响——它的 handler 走的是 contextmenu，按钮 (button=0) 不在这一支上。
    #[cfg(target_arch = "wasm32")]
    {
        let exit_handle = window_event_listener(leptos::ev::mousedown, move |ev| {
            if editing_cell.get_untracked().is_none() {
                return;
            }
            if ev.button() != 0 {
                // 只对左键反应：中 / 右键进入下一阶段（contextmenu），交给 toggle_edit 自己处理。
                return;
            }
            let in_edit = ev.composed_path().iter().any(|node| {
                node.dyn_ref::<leptos::web_sys::Element>()
                    .is_some_and(|el| {
                        el.class_list().contains("lblcell")
                            && el.class_list().contains("editing")
                    })
            });
            if !in_edit {
                editing_cell.set(None);
            }
        });
        on_cleanup(move || exit_handle.remove());
    }
    // 持久多选模式的两条退出路径：
    // 1) ESC 键退出。
    // 2) 点表格外区域退出——`.tbl-scroll` 是表格滚动容器（含表格本体 + 分页器之外的列头），
    //    命中它就不退出；其它位置（视图导航栏 / 过滤器 / 已归档弹窗 / 详情面板 / 窗口边缘）都退出。
    //    仅挂在 wasm 目标下——SSR 编译时闭包不生成。
    #[cfg(target_arch = "wasm32")]
    {
        let mode_for_esc = multi_select_mode;
        let esc_handle = window_event_listener(leptos::ev::keydown, move |ev: leptos::ev::KeyboardEvent| {
            if !mode_for_esc.get_untracked() {
                return;
            }
            if ev.key() == "Escape" {
                mode_for_esc.set(false);
            }
        });
        on_cleanup(move || esc_handle.remove());
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mode_for_outside = multi_select_mode;
        let outside_handle = window_event_listener(leptos::ev::mousedown, move |ev| {
            if !mode_for_outside.get_untracked() {
                return;
            }
            // 只要点击目标在 `.tbl-scroll` 之内就保留模式；表格之外一律退出。
            // 用 composed_path 走完整冒泡路径，跟 editing_cell 那条监听同款——
            // 避免 Chrome 在 `<span>text</span>` 上偶发把 target 设成 Text 节点。
            let in_scroll = ev.composed_path().iter().any(|node| {
                node.dyn_ref::<leptos::web_sys::Element>()
                    .is_some_and(|el| el.class_list().contains("tbl-scroll"))
            });
            if !in_scroll {
                mode_for_outside.set(false);
            }
        });
        on_cleanup(move || outside_handle.remove());
    }
    // 可排序表头：内置三列恒可排；标签列只在其属于本视图 `columns` 时可排——
    // 列不展示的标签排序，用户看不见结果。
    let sortable_th = move |field: String, label: String| {
        let click_field = field.clone();
        let dbl_field = field.clone();
        view! {
            <th class="sortable"
                on:click=move |ev: leptos::ev::MouseEvent| {
                    // 双击的第二次 click（detail==2）由 dblclick 处理，否则与单击冲突。
                    if ev.detail() > 1 { return; }
                    on_sort.run(SortRequest {
                        field: click_field.clone(),
                        additive: ev.shift_key(),
                        remove: false,
                    });
                }
                on:dblclick=move |_| {
                    on_sort.run(SortRequest {
                        field: dbl_field.clone(),
                        additive: false,
                        remove: true,
                    });
                }
            >
                {move || format!("{label}{}", sort_mark(&sorts.get(), &field))}
            </th>
        }
    };

    // 当前列表里的条目 code；「全选」按它来，只影响看得见的行。
    let visible_codes = move || -> Vec<String> {
        data.get()
            .and_then(|r| r.ok())
            .map(|(_, items, _)| items.into_iter().map(|e| e.code).collect())
            .unwrap_or_default()
    };

    // 当前页的评论计数。只取屏幕上这几行的 code——服务端按 code 逐个前缀扫描，
    // 不需要工作空间级的评论索引列族。
    let counts = RwSignal::new(std::collections::HashMap::<String, i32>::new());
    Effect::new(move |_| {
        let codes = visible_codes();
        if codes.is_empty() || logged_out() {
            return;
        }
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                if let Ok(list) = crate::frontend::graphql_client::comment_counts(&codes).await {
                    counts.set(list.into_iter().collect());
                }
            });
        }
    });

    view! {
        <div class="tbl-scroll"
            on:wheel=move |ev| {
                // Shift + 滚轮 → 横向滚动：命中时 prevent_default 并把 delta_y
                // 转到 scrollLeft；没按 Shift 不干预，浏览器按默认竖直走，行为与
                // 无 modifier 一致，竖向滚动条不受影响。cast 进 HtmlElement 的 JsCast
                // impl 仅 wasm 目标存在，整个 helper 也按 wasm32 cfg 切。
                if ev.shift_key() && ev.delta_y() != 0.0 {
                    ev.prevent_default();
                    #[cfg(target_arch = "wasm32")]
                    crate::frontend::wheel::shift_wheel_horizontal(&ev);
                }
            }
        >
        <table class=move || if multi_select_mode.get() { "tbl multi-mode" } else { "tbl" }>
            <thead>
                <tr>
                    <th class="pick">
                        <input type="checkbox"
                            title=i18n.t("entry.select_all")
                            prop:checked=move || {
                                let codes = visible_codes();
                                !codes.is_empty()
                                    && codes.iter().all(|c| batch_selected.get().contains(c))
                            }
                            on:change=move |ev| {
                                let codes = visible_codes();
                                let on = event_target_checked(&ev);
                                batch_selected.update(|sel| {
                                    if on {
                                        for c in codes {
                                            if !sel.contains(&c) {
                                                sel.push(c);
                                            }
                                        }
                                    } else {
                                        sel.retain(|c| !codes.contains(c));
                                    }
                                });
                            }
                        />
                    </th>
                    {sortable_th("title".to_string(), i18n.t("entry.col_title"))}
                    {move || cols().iter().map(|name| {
                        // 4 个元信息列走 sentinel：表头按 builtin 排序字段渲染（与
                        // `domain::view::SortField::from_str` 对齐）；元信息单元格只在视图
                        // 显式勾选时才出现——默认视图不带它们，避免「更新时间是表格的常驻列」。
                        if let Some(field) = metadata_sort_field(name) {
                            let label = match field {
                                "createdBy" => i18n.t("entry.col_created_by"),
                                "createdAt" => i18n.t("entry.col_created_at"),
                                "updatedBy" => i18n.t("entry.col_updated_by"),
                                _ => i18n.t("entry.col_updated_at"),
                            };
                            sortable_th(field.to_string(), label)
                        } else {
                            let title = schemas.get().into_iter()
                                .find(|s| &s.name == name)
                                .map(|s| s.title)
                                .unwrap_or_else(|| name.clone());
                            sortable_th(name.clone(), title)
                        }
                    }).collect::<Vec<_>>()}
                </tr>
            </thead>
            <tbody>
                {move || match data.get() {
                    None => view! {
                        <tr><td colspan="20" class="empty">{i18n.t("common.loading")}</td></tr>
                    }.into_any(),
                    Some(Err(e)) => view! {
                        <tr><td colspan="20" class="empty error">{e.clone()}</td></tr>
                    }.into_any(),
                    Some(Ok((_ws, items, _))) if items.is_empty() => view! {
                        <tr><td colspan="20" class="empty">{i18n.t("entry.empty_hint")}</td></tr>
                    }.into_any(),
                    Some(Ok((_ws, items, _))) => {
                        let sc = schemas.get();
                        // 账号列展示用：把值里的 id 映射成成员姓名。
                        let members = members.get();
                        items.iter().enumerate().map(|(row_idx, e)| {
                            // 当前页里的行号（从 1 起算）作为选 / 排序时回看的视觉锚点。
                            // 行号随翻页变化——不持久化进批量勾选，与「勾选状态属于哪些条目」分离。
                            let row_no = row_idx + 1;
                            let code_for_class = e.code.clone();
                            // 给 `<tr>` 上的 on:click 单独一份：class 闭包已经把 code_for_class
                            // move 走了，这里的非 move 闭包拿不到原值，会编译失败。
                            let code_for_row_click = e.code.clone();
                            let code_for_click = e.code.clone();
                            let code_for_dbl = e.code.clone();
                            let code_for_check = e.code.clone();
                            let code_for_check_change = e.code.clone();
                            let code_for_badge = e.code.clone();
                            let labels = e.labels.clone();
                            // 继承 / 覆盖推导：只影响展示（虚线 + 来源说明），不改数据。
                            let derived = query_eval::derive_inherited(&sc, &e.labels);
                            // 标题着色：整行 Entry 克隆进响应式闭包，规则变化即刻重算。
                            let entry_for_color = e.clone();
                            // 时间型标签的比较需要 schema（布局），随规则闭包一起克隆。
                            let schemas_for_color = sc.clone();
                            let title_text = e.title.clone();
                            let names = cols();
                            // 创建人列按视图配置开关：仅当 sentinel 在 `columns` 里时才计算与渲染，
                            // 默认视图不带它——spec 要求「默认不展示创建人」。
                            let show_creator = names.iter().any(|n| n.as_str() == COLUMN_CREATED_BY);
                            // 创建人展示：优先服务端回填的账号，其次在工作空间成员表里找名字，
                            // 账号已删除或已退出工作空间时退回 id，好过显示成空。
                            let creator = if show_creator {
                                e.created_by_account
                                    .as_ref()
                                    .map(|a| a.name.clone())
                                    .or_else(|| {
                                        members
                                            .iter()
                                            .find(|m| m.account_id == e.created_by)
                                            .map(member_label)
                                    })
                                    .unwrap_or_else(|| "—".to_string())
                            } else {
                                String::new()
                            };
                            view! {
                                <tr
                                    class=move || if selected.get() == code_for_class { "sel".to_string() } else { String::new() }
                                    // 点行上除标题 / 复选框 / 编辑态单元格以外的任何位置都
                                    // 等价于「只想选中，不开详情」：翻转行的 `.sel` 背景。
                                    // 标题单元格与复选框各自 stop_propagation 阻止冒泡到这里，
                                    // 编辑态单元格同理（见下面 `<td class="lblcell editing">`）。
                                    // 不动 `panel_code`——只有标题点击才会打开右侧详情面板。
                                    //
                                    // 多选模式下：点行只切 batch_selected，不动 selected /
                                    // panel_code——避免「点了反而把右侧面板撬开 / 行高亮跳走」。
                                    on:click=move |_| {
                                        if multi_select_mode.get_untracked() {
                                            let c = code_for_row_click.clone();
                                            batch_selected.update(|s| {
                                                if let Some(pos) = s.iter().position(|x| x == &c) {
                                                    s.remove(pos);
                                                } else {
                                                    s.push(c);
                                                }
                                            });
                                            return;
                                        }
                                        if selected.get_untracked() == code_for_row_click {
                                            selected.set(String::new());
                                        } else {
                                            selected.set(code_for_row_click.clone());
                                        }
                                    }
                                >
                                    <td class="pick">
                                        // 右键行序号 → 切 batch_selected + 进入持久多选模式。
                                        // handler 挂在 `<label>` 而不是 `<span class="row-no">` 上：
                                        // CSS 把 row-no 设为 `pointer-events: none`（让行号不挡复选框点击），
                                        // 右键事件会冒泡到 label。整格右键都触发同样动作——用户视觉锚点在「行号」，
                                        // 但事件落地在外层 label，更稳。
                                        // 光标与复选框的可见性由 `<table class="tbl multi-mode?">` 统一切换，
                                        // 这里不再需要 `pick-hint`。
                                        <label class="pick-cell"
                                            title=i18n.tf("entry.page_row_n", &[("n", &row_no.to_string())])
                                            // 多选模式下拦截 label 自身的 click 冒泡：label 是 checkbox 的关联控件，
                                            // 点 label 会触发两路 click——label 自身的 click 冒泡到 `<tr>`，
                                            // 以及浏览器合成的 checkbox click（带 stop_propagation）。
                                            // 不拦的话两路都会切 batch_selected，叠加后净效果为 0。
                                            // 非多选态不拦：保留「点 label 区域也能翻 selected 行高亮」的旧行为。
                                            on:click=move |ev| {
                                                if multi_select_mode.get_untracked() {
                                                    ev.stop_propagation();
                                                }
                                            }
                                            on:contextmenu=move |ev: leptos::ev::MouseEvent| {
                                                ev.prevent_default();
                                                ev.stop_propagation();
                                                let c = code_for_check_change.clone();
                                                batch_selected.update(|s| {
                                                    if let Some(pos) = s.iter().position(|x| x == &c) {
                                                        s.remove(pos);
                                                    } else {
                                                        s.push(c);
                                                    }
                                                });
                                                multi_select_mode.set(true);
                                            }
                                        >
                                            <input type="checkbox"
                                                prop:checked={
                                            let c = code_for_check.clone();
                                            move || batch_selected.get().contains(&c)
                                        }
                                                // 勾选不应触发「打开详情」（单击）或「全屏页」（双击）。
                                                on:click=|ev| ev.stop_propagation()
                                                on:dblclick=|ev| ev.stop_propagation()
                                                on:change={
                                                    // `on:contextmenu` 上面那个 `move` 闭包已经把
                                                    // `code_for_check_change` move 走了一次；这里要再
                                                    // 用就得分一份独立的 owned 副本——在块表达式里
                                                    // 提前 clone，move 闭包只拿这份副本，原变量留在外面
                                                    // 给后续渲染继续用。
                                                    let v = code_for_check_change.clone();
                                                    move |ev| {
                                                        let c = v.clone();
                                                        if event_target_checked(&ev) {
                                                            batch_selected.update(|s| if !s.contains(&c) { s.push(c.clone()) });
                                                        } else {
                                                            batch_selected.update(|s| s.retain(|x| x != &c));
                                                        }
                                                    }
                                                }
                                            />
                                            // 行号：用 <span> 而不是 input 自身的 attribute，避免被读屏器念成
                                            // 「第 1 行 可勾选 复选框 1」这种多余组合。
                                            // 右键 handler 挂在外层 `<label>` 上（CSS 给 row-no 设了
                                            // `pointer-events: none`，事件不会落到这里，落到 label）。
                                            // 多选模式时 class 加 `multi-mode`，CSS 把它换成主色加粗——给用户
                                            // 一眼可辨的「现在点行就只切多选」的视觉提示。
                                            <span class=move || if multi_select_mode.get() { "row-no multi-mode" } else { "row-no" }>{row_no.to_string()}</span>
                                        </label>
                                    </td>
                                    <td class="title-cell"
                                    style=move || match query_eval::title_color(
                                        &title_colors.get(), &entry_for_color, &entry_for_color.labels,
                                        &schemas_for_color,
                                    ) {
                                        Some(c) => format!("color:{c}"),
                                        None => String::new(),
                                    }
                                    on:click=move |ev: leptos::ev::MouseEvent| {
                                        // 双击的第二次 click（detail==2）交给 dblclick 处理，
                                        // 避免单击开合与双击互斥冲突。
                                        if ev.detail() > 1 { return; }
                                        // 多选模式下：点标题也只切 batch_selected，不开面板。
                                        // stop_propagation 避免点完标题又被 `<tr>` 的 on:click 翻一次——
                                        // 两边逻辑对称，但叠加会把这次勾选再撤销掉。
                                        if multi_select_mode.get_untracked() {
                                            ev.stop_propagation();
                                            let c = code_for_click.clone();
                                            batch_selected.update(|s| {
                                                if let Some(pos) = s.iter().position(|x| x == &c) {
                                                    s.remove(pos);
                                                } else {
                                                    s.push(c);
                                                }
                                            });
                                            return;
                                        }
                                        // 标题点击：行高亮永远置为该行，**不再翻手取消**——
                                        // 无论点击多少次，同一行的 `.sel` 高亮都得保持（与
                                        // 「该 Entry 始终处于选中状态」的需求对齐）。
                                        // `panel_code` 单独切换开关：点同一行标题开 / 关
                                        // 详情面板，让用户关掉面板时高亮仍在。
                                        // 其他列点击走 `<tr>` 上的 on:click，只翻 `selected`，
                                        // 不动 `panel_code`。
                                        // stop_propagation 避免 `<tr>` 的 on:click 再翻一次。
                                        ev.stop_propagation();
                                        selected.set(code_for_click.clone());
                                        if panel_code.get_untracked() == code_for_click {
                                            panel_code.set(String::new());
                                        } else {
                                            panel_code.set(code_for_click.clone());
                                        }
                                    }
                                    on:dblclick=move |_| {
                                        on_open.run(code_for_dbl.clone());
                                    }
                                >{title_text.clone()}{counts
                                        .get()
                                        .get(&code_for_badge)
                                        .copied()
                                        .filter(|n| *n > 0)
                                        .map(|n| view! {
                                            <span class="c-badge" title=i18n.t("entry.comment_count")>
                                                {ic_comment()}{n}
                                            </span>
                                        })}</td>
                                    {names.iter().map(|name| {
                                        if let Some(field) = metadata_sort_field(name) {
                                            // 4 个元信息列的单元格：账号型（创建人 / 更新人）展示姓名
                                            // （账号已删时退回 id，避免空字符串），时间型走与详情面板
                                            // 同源的 `short_time`；元信息列不参与内联编辑，纯净展示。
                                            match field {
                                                "createdBy" => {
                                                    return view! { <td class="mut">{creator.clone()}</td> }.into_any();
                                                }
                                                "createdAt" => {
                                                    let at = e.created_at.clone();
                                                    return view! { <td class="mut">{short_time(&at)}</td> }.into_any();
                                                }
                                                "updatedBy" => {
                                                    // 与创建人同一份查找逻辑：服务端回填优先，成员表兜底。
                                                    let name = e
                                                        .updated_by_account
                                                        .as_ref()
                                                        .map(|a| a.name.clone())
                                                        .or_else(|| {
                                                            members
                                                                .iter()
                                                                .find(|m| m.account_id == e.updated_by)
                                                                .map(member_label)
                                                        })
                                                        .unwrap_or_else(|| "—".to_string());
                                                    return view! { <td class="mut">{name}</td> }.into_any();
                                                }
                                                _ => {
                                                    let at = e.updated_at.clone();
                                                    return view! { <td class="mut">{short_time(&at)}</td> }.into_any();
                                                }
                                            }
                                        }
                                        // 优先直接打标；没有再看继承 / 覆盖推导来的（`source` 非空即为推导所得）。
                                        let direct = labels.iter()
                                            .find(|l| &l.label_name == name)
                                            .map(|l| l.value.clone());
                                        let lv = direct.map(|v| (v, Option::<String>::None)).or_else(|| {
                                            derived.iter()
                                                .find(|(n, _, _)| n == name)
                                                .map(|(_, v, src)| (v.clone(), Some(src.clone())))
                                        });
                                        let schema_for_edit = sc.iter().find(|sch| &sch.name == name).cloned();
                                        // 编辑态：本格子的 (entry_code, label_name) 是当前唯一的编辑目标就切到编辑器。
                                        // 内联编辑与「只有标题能选中行」的规则相容——chip 点击只走 on:click→editing_cell，
                                        // 不触发任何与 selected 相关的逻辑，避免误开侧栏详情。
                                        let editing_now = editing_cell.get().as_ref()
                                            == Some(&(code_for_check.clone(), name.clone()));
                                        // chip 进入编辑态的统一切换：右键切到本格；同一格再右键视为「退出」。
                                        // `prevent_default` 屏蔽浏览器自带的右键菜单——这一下不是来要菜单的。
                                        let name_for_click = name.clone();
                                        let code_for_click_chip = code_for_check.clone();
                                        let toggle_edit = move |ev: leptos::ev::MouseEvent| {
                                            ev.prevent_default();
                                            ev.stop_propagation();
                                            let editing = editing_cell.get_untracked();
                                            let me = (code_for_click_chip.clone(), name_for_click.clone());
                                            if editing.as_ref() == Some(&me) {
                                                editing_cell.set(None);
                                            } else {
                                                editing_cell.set(Some(me));
                                            }
                                        };
                                        if editing_now {
                                            // 编辑器接管单元格：tag 定义被删掉时编辑器也无法工作，给一个空态文本。
                                            let cur_for_edit = lv.as_ref().map(|(v, _)| v.clone());
                                            let name_for_edit = name.clone();
                                            let code_for_edit = code_for_check.clone();
                                            let ms_for_edit = members.clone();
                                            let close_edit: Callback<()> = Callback::new(move |_: ()| {
                                                editing_cell.set(None);
                                                on_label_changed.run(());
                                            });
                                            return match schema_for_edit {
                                                Some(s) => view! {
                                                    <td class="lblcell editing"
                                                        on:click=|ev| ev.stop_propagation()>
                                                        <LabelCellEditor
                                                            code=code_for_edit
                                                            name=name_for_edit
                                                            schema=s
                                                            current=cur_for_edit
                                                            members=ms_for_edit
                                                            on_done=close_edit />
                                                    </td>
                                                }.into_any(),
                                                None => view! {
                                                    <td class="lblcell editing"
                                                        on:click=|ev| ev.stop_propagation()>
                                                        <div class="mut">"label removed"</div>
                                                    </td>
                                                }.into_any(),
                                            };
                                        }
                                        match lv {
                                            None => {
                                                // 未打标：在表格里呈现「—」，右键进入编辑态——补打标签的最常见入口。
                                                let toggle = toggle_edit.clone();
                                                view! {
                                                    <td class="mut clickable"
                                                        on:contextmenu=move |ev: leptos::ev::MouseEvent| toggle(ev)>
                                                        "—"
                                                    </td>
                                                }.into_any()
                                            }
                                            Some((v, source)) => {
                                                // 多值（数组）逐元素走内置枚举的友好展示，再拼接；
                                                // 否则 display_enum_value 只作用在整串上，内层 token 漏掉。
                                                let s = match &v {
                                                    Value::Array(a) => a
                                                        .iter()
                                                        .map(|x| display_enum_value(&value_to_string(x)))
                                                        .collect::<Vec<_>>()
                                                        .join(", "),
                                                    other => value_to_string(other),
                                                };
                                                let schema = sc.iter().find(|sch| &sch.name == name);
                                                let is_enum = schema
                                                    .is_some_and(|sch| sch.value_type == "enum");
                                                let is_null = schema
                                                    .is_some_and(|sch| sch.value_type == "null");
                                                let is_account = schema
                                                    .is_some_and(|sch| sch.value_type == "account");
                                                // 无值标签没有可展示的值，退而展示标签标题。
                                                let text = if is_null {
                                                    schema
                                                        .map(|sch| {
                                                            if sch.title.trim().is_empty() {
                                                                name.clone()
                                                            } else {
                                                                sch.title.clone()
                                                            }
                                                        })
                                                        .unwrap_or_else(|| name.clone())
                                                } else if is_account {
                                                    // 单值：存的是账号 id；多值：存的字符串数组，逐元素映射。
                                                    if let Some(arr) = v.as_array() {
                                                        arr.iter()
                                                            .map(|x| {
                                                                let id = value_to_string(x);
                                                                members
                                                                    .iter()
                                                                    .find(|m| m.account_id == id)
                                                                    .map(member_label)
                                                                    .unwrap_or_else(|| id.clone())
                                                            })
                                                            .collect::<Vec<_>>()
                                                            .join(", ")
                                                    } else {
                                                        members
                                                            .iter()
                                                            .find(|m| m.account_id == s)
                                                            .map(member_label)
                                                            .unwrap_or_else(|| s.clone())
                                                    }
                                                } else {
                                                    display_enum_value(&s)
                                                };
                                                // 类型标点：扫一列 chip 时给眼睛一个锚点——日期 / 金额 / 账号 /
                                                // boolean 一眼可分辨，不必读完 chip 全部内容。Boolean 标点
                                                // 就是它的值（✓ / ○），text body 留空避免「true ✓」这种堆叠。
                                                let vt = schema
                                                    .map(|sch| sch.value_type.as_str())
                                                    .unwrap_or("");
                                                let glyph = schema.and_then(|sch| {
                                                    chip_glyph(
                                                        &sch.value_type,
                                                        &v,
                                                        sch.currency_symbol.as_deref(),
                                                    )
                                                });
                                                let body = if vt == "boolean" {
                                                    String::new()
                                                } else {
                                                    text.clone()
                                                };
                                                let glyph_left =
                                                    matches!(glyph, Some((_, GlyphPos::Left)));
                                                let glyph_right =
                                                    matches!(glyph, Some((_, GlyphPos::Right)));
                                                let glyph_char = glyph.map(|(g, _)| g);
                                                // 值色优先（标签级配置），未配置回退 label_chip_class。
                                                let color = schema.and_then(|sch| {
                                                    let base = sch.color.clone().map(Value::String);
                                                    query_eval::resolve_label_color(
                                                        base.as_ref(), &sch.value_colors, &v,
                                                    )
                                                });
                                                // 推导得来的标签在表格里要能一眼看出来：虚线描边 + 悬停说明来源。
                                                let hint = source
                                                    .as_ref()
                                                    .map(|src| i18n.tf("label.derived_title", &[("src", &src)]));
                                                let inh = source.is_some();
                                                match color {
                                                    Some(c) => {
                                                        let style = format!(
                                                            "border-color:{c};background:color-mix(in srgb, {c} 15%, transparent);color:{c}"
                                                        );
                                                        let cls = if inh { "chip inh" } else { "chip" };
                                                        let cursor_cls = format!("{cls} clickable");
                                                        let toggle = toggle_edit.clone();
                                                        view! {
                                                            <td><span class=cursor_cls style=style title=hint
                                                                on:contextmenu=move |ev: leptos::ev::MouseEvent| toggle(ev)>
                                                                {glyph_left.then(|| view! { <span class="chip-glyph">{glyph_char.clone().unwrap_or_default()}</span> })}
                                                                {body.clone()}
                                                                {glyph_right.then(|| view! { <span class="chip-glyph">{glyph_char.clone().unwrap_or_default()}</span> })}
                                                            </span></td>
                                                        }.into_any()
                                                    }
                                                    None if is_enum => {
                                                        let cls = label_chip_class(name, &s);
                                                        let cls = if inh { format!("chip {cls} inh") } else { format!("chip {cls}") };
                                                        let cursor_cls = format!("{cls} clickable");
                                                        let toggle = toggle_edit.clone();
                                                        view! {
                                                            <td><span class=cursor_cls title=hint
                                                                on:contextmenu=move |ev: leptos::ev::MouseEvent| toggle(ev)>
                                                                {glyph_left.then(|| view! { <span class="chip-glyph">{glyph_char.clone().unwrap_or_default()}</span> })}
                                                                {body.clone()}
                                                                {glyph_right.then(|| view! { <span class="chip-glyph">{glyph_char.clone().unwrap_or_default()}</span> })}
                                                            </span></td>
                                                        }.into_any()
                                                    }
                                                    None if is_null => {
                                                        let cls = if inh { "chip inh" } else { "chip" };
                                                        let cursor_cls = format!("{cls} clickable");
                                                        let toggle = toggle_edit.clone();
                                                        view! { <td><span class=cursor_cls title=hint
                                                            on:contextmenu=move |ev: leptos::ev::MouseEvent| toggle(ev)>
                                                            {glyph_left.then(|| view! { <span class="chip-glyph">{glyph_char.clone().unwrap_or_default()}</span> })}
                                                            {body.clone()}
                                                            {glyph_right.then(|| view! { <span class="chip-glyph">{glyph_char.clone().unwrap_or_default()}</span> })}
                                                        </span></td> }.into_any()
                                                    }
                                                    None => {
                                                        // 无 chip 包裹的兜底分支（整数 / 浮点 / 字符串 / 时间型），
                                                        // 也带上标点——日期「◷ 09-12 14:30」和金额「¥ 1200」在此处拼出来。
                                                        let toggle = toggle_edit.clone();
                                                        view! { <td class="clickable" title=hint
                                                            on:contextmenu=move |ev: leptos::ev::MouseEvent| toggle(ev)>
                                                            {glyph_left.then(|| view! { <span class="chip-glyph">{glyph_char.clone().unwrap_or_default()}</span> })}
                                                            {body.clone()}
                                                            {glyph_right.then(|| view! { <span class="chip-glyph">{glyph_char.clone().unwrap_or_default()}</span> })}
                                                        </td> }.into_any()
                                                    }
                                                }
                                            }
                                        }
                                    }).collect::<Vec<_>>()}
                                </tr>
                            }
                        }).collect::<Vec<_>>().into_any()
                    }
                }}
            </tbody>
        </table>
        </div>
    }
}

/// 表格单元格的内联标签编辑器：仅在 EntryTable 单元格进入编辑态时挂载。
///
/// 与 `label_editor::LabelRow` 的编辑控件分支基本同构（同一个 LabelEditor 是「值控件 +
/// 删除按钮」全行；这里只剩值控件）。独立成组件而不是直接复用 `LabelRow` 是因为后者把标题、
/// 删除按钮、配色都打在外层 div 上，强行塞进单元格里会与表格自身的列对齐 / 配色规则互相踩
/// ——把共有的「按 value_type 切控件」部分提走，下游再合用。
///
/// 退出由外层 EntryTable 的全局 mousedown 监听负责（左键落在 `.lblcell.editing` 之外），
/// 这里不再渲染「↩ 返回」按钮。
///
/// 视图表格里**不允许移除标签**：清空输入时不写入（保留原值）；无值标签只能「打上」、不能
/// 移除。删除须到详情面板用完整 LabelEditor。
#[component]
fn LabelCellEditor(
    /// 当前条目 code。落地要它，不能一次解锁。
    code: String,
    /// 标签 name；与 code 一起组成 `set_labeling` 的双键。
    name: String,
    /// 标签定义，决定出哪种控件。
    schema: LabelSchema,
    /// 当前值（不打标时 `None`，无值标签的「打上」按 `null` 处理）。
    current: Option<Value>,
    /// 工作空间成员表：Account 型标签从这里挑。
    members: Vec<Member>,
    /// 落地成功（与失败）都通知外层——让外层重拉条目表拿到最新值；编辑态同时关闭。
    /// 失败也关：与「详情面板的 LabelEditor 没有失败分支 inline 提示」一致，不阻塞别的操作。
    on_done: Callback<()>,
) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    // Local last user-typed text for branches that defer save to a button (email / currency /
    // string/integer/float)。与 LabelRow 那边的实现一致，避免边输入边请求把偶发的空串也写回。
    let current_text = current.as_ref().map(value_to_string).unwrap_or_default();
    // 真正的「写」动作：把出错落到末尾集中显示，LabelEditor 的处理也是这一套。
    let local_err: RwSignal<Option<String>> = RwSignal::new(None);
    let on_set: Callback<Value> = {
        let c = code.clone();
        let n = name.clone();
        Callback::new(move |v: Value| {
            let c = c.clone();
            let n = n.clone();
            spawn_local(async move {
                let res = set_labeling(&c, &n, &v).await;
                local_err.set(res.err());
                on_done.run(());
            });
        })
    };

    view! {
        // 屏蔽浏览器右键菜单：编辑态不是来要菜单的；左键 stop_propagation 留给下面那些
        // 控件自己用，免得点空白处冒到外层 td 把当前编辑关掉（外层 td 不再监听点击，
        // 这一点当前行为没在用，但保留显式 stop_propagation 以防未来加 row 级点击）。
        <div class="lblcell-edit"
            on:contextmenu=|ev| ev.prevent_default()
            on:click=|ev| ev.stop_propagation()>
            {match &schema {
                // 无值标签：未打标时允许「打上」（写 null）；已打标时无值可编辑，也不允许移除——
                // 这里空着，让外层详情面板去删。空 schema 不应该进编辑态，但万一进了也安全收尾。
                s if s.value_type == "null" => {
                    let cur_is_none = current.is_none();
                    if cur_is_none {
                        let on_set = on_set.clone();
                        view! {
                            <button class="btn sm" on:click=move |_| on_set.run(Value::Null)>
                                {move || i18n.t("label.set")}
                            </button>
                        }.into_any()
                    } else {
                        view! { <span class="mut">{move || i18n.t("label.no_value_hint")}</span> }
                        .into_any()
                    }
                }
                // 多选 Enum：勾选集合整体写成一个数组。
                s if s.value_type == "enum" && s.multi => {
                    let opts = s.enum_values.clone();
                    let selected: std::collections::HashSet<String> = current
                        .as_ref()
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let current_arc = current.clone();
                    view! {
                        <div class="multienum">
                            {opts.into_iter().map(|o| {
                                let checked = selected.contains(&o);
                                let cur = current_arc.clone();
                                let oc = o.clone();
                                let on_set = on_set.clone();
                                view! {
                                    <label class="mut">
                                        <input type="checkbox" prop:checked=checked on:change=move |ev| {
                                            let mut set: Vec<String> = cur
                                                .as_ref()
                                                .and_then(|v| v.as_array())
                                                .map(|a| {
                                                    a.iter()
                                                        .filter_map(|x| x.as_str().map(str::to_string))
                                                        .collect()
                                                })
                                                .unwrap_or_default();
                                            if event_target_checked(&ev) {
                                                if !set.contains(&oc) {
                                                    set.push(oc.clone());
                                                }
                                            } else {
                                                set.retain(|x| x != &oc);
                                            }
                                            // 不支持移除：全空时不写入，保留原值。
                                            if !set.is_empty() {
                                                on_set
                                                    .run(
                                                        Value::Array(
                                                            set.into_iter().map(Value::String).collect(),
                                                        ),
                                                    );
                                            }
                                        } />
                                        {display_enum_value(&o)}
                                    </label>
                                }
                            }).collect::<Vec<_>>()}
                        </div>
                    }.into_any()
                }
                s if s.value_type == "enum" => {
                    let opts = s.enum_values.clone();
                    let cur_text = current_text.clone();
                    let on_set = on_set.clone();
                    view! {
                        <select on:change=move |ev| {
                            let v = event_target_value(&ev);
                            if !v.is_empty() {
                                on_set.run(Value::String(v));
                            }
                        }>
                            <option value="" selected=cur_text.is_empty() disabled>
                                {move || i18n.t("label.select_placeholder")}
                            </option>
                            {opts.into_iter().map(|o| {
                                let cur_text = cur_text.clone();
                                view! {
                                    <option value=o.clone() selected=cur_text == o>
                                        {display_enum_value(&o)}
                                    </option>
                                }
                            }).collect::<Vec<_>>()}
                        </select>
                    }.into_any()
                }
                s if s.value_type == "boolean" => {
                    let checked = current_text == "true";
                    let on_set = on_set.clone();
                    view! {
                        <input type="checkbox" prop:checked=checked on:change=move |ev| {
                            on_set.run(Value::Bool(event_target_checked(&ev)));
                        } />
                    }.into_any()
                }
                s if matches!(s.value_type.as_str(), "date" | "time" | "datetime") => {
                    let vt = s.value_type.clone();
                    let layout = crate::golayout::resolve(
                        s.format.as_deref(),
                        crate::golayout::default_go(&vt),
                    );
                    let native_ok = is_native_time_layout(&vt, s.format.as_deref());
                    if !native_ok {
                        let inp = RwSignal::new(current_text.clone());
                        let layout_cb = layout.clone();
                        let on_set = on_set.clone();
                        view! {
                            <input class="inp" style="width:140px" prop:value=inp
                                on:input=move |ev| inp.set(event_target_value(&ev))
                                on:change=move |ev| {
                                    let v = event_target_value(&ev);
                                    inp.set(v.clone());
                                    // 不支持移除：清空时不写入，保留原值。
                                    if !v.is_empty()
                                        && crate::golayout::parse(&layout_cb, &v).is_some()
                                    {
                                        on_set.run(Value::String(v));
                                    }
                                } />
                        }.into_any()
                    } else {
                        let input_type = match vt.as_str() {
                            "date" => "date",
                            "time" => "time",
                            _ => "datetime-local",
                        };
                        let current_native = to_native(&vt, &current_text);
                        let on_set = on_set.clone();
                        view! {
                            <input class="inp" type=input_type prop:value=current_native
                                on:change=move |ev| {
                                    let v = event_target_value(&ev);
                                    // 不支持移除：清空时不写入。
                                    if !v.is_empty() {
                                        if let Some(stored) = from_native(&vt, &v) {
                                            on_set.run(Value::String(stored));
                                        }
                                    }
                                } />
                        }.into_any()
                    }
                }
                s if s.value_type == "currency" => {
                    let inp = RwSignal::new(current_text.clone());
                    let prefix = format!(
                        "{}{}",
                        s.currency_symbol.clone().unwrap_or_else(|| "¥".to_string()),
                        s.unit.clone().map(|u| format!(" {u}")).unwrap_or_default()
                    );
                    let on_set = on_set.clone();
                    view! {
                        <div style="display:flex;gap:6px;align-items:center">
                            <span class="mut">{prefix}</span>
                            <input class="inp" style="width:90px" type="number" prop:value=inp
                                on:input=move |ev| inp.set(event_target_value(&ev)) />
                            <button class="btn" on:click=move |_| {
                                let v = inp.get_untracked();
                                // 不支持移除：清空时不写入，保留原值。
                                if v.is_empty() {
                                    return;
                                }
                                if let Some(n) = v
                                    .parse::<f64>()
                                    .ok()
                                    .and_then(serde_json::Number::from_f64)
                                {
                                    on_set.run(Value::Number(n));
                                }
                            }>{move || i18n.t("label.set")}</button>
                        </div>
                    }.into_any()
                }
                s if s.value_type == "account" => {
                    let s_multi = s.multi;
                    let picked = {
                        let on_set = on_set.clone();
                        Callback::new(move |ids: Vec<String>| {
                            if s_multi {
                                // 不支持移除：清空选择时不写入，保留原值。
                                if !ids.is_empty() {
                                    on_set.run(Value::Array(
                                        ids.into_iter().map(Value::String).collect(),
                                    ));
                                }
                            } else {
                                match ids.into_iter().next() {
                                    Some(id) => on_set.run(Value::String(id)),
                                    // 不支持移除：不写入，保留原值。
                                    None => {}
                                }
                            }
                        })
                    };
                    // AccountPicker 现在按 Signal<Vec<String>> 读 selected；外面 on_set
                    // 触发本组件重渲时 current_text 不变（仍是上次读到的字符串），但
                    // Signal::derive 让 AccountPicker 内部 chip 随 selected 变化刷新。
                    let selected = Signal::derive(move || {
                        if s.multi {
                            current_text
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect()
                        } else if current_text.is_empty() {
                            Vec::new()
                        } else {
                            vec![current_text.clone()]
                        }
                    });
                    view! {
                        <AccountPicker members=members.clone() selected=selected multi=s.multi
                            on_change=picked />
                    }.into_any()
                }
                s if s.value_type == "email" => {
                    let inp = RwSignal::new(current_text.clone());
                    let on_set = on_set.clone();
                    view! {
                        <div style="display:flex;gap:6px">
                            <input class="inp" style="width:160px" type="email" prop:value=inp
                                on:input=move |ev| inp.set(event_target_value(&ev)) />
                            <button class="btn" on:click=move |_| {
                                let v = inp.get_untracked();
                                // 不支持移除：清空时不写入，保留原值。
                                if v.is_empty() {
                                    return;
                                }
                                if v.contains('@') {
                                    on_set.run(Value::String(v));
                                }
                            }>{move || i18n.t("label.set")}</button>
                        </div>
                    }.into_any()
                }
                _ => {
                    // 整数 / 浮点 / 字符串的统一处理：input + 「设置」按钮，留住半成品。
                    let s_typed: LabelSchema = schema.clone();
                    let vt = s_typed.value_type.clone();
                    let inp = RwSignal::new(current_text.clone());
                    let on_set = on_set.clone();
                    view! {
                        <div style="display:flex;gap:6px">
                            <input class="inp" style="width:110px"
                                type=if vt == "string" { "text" } else { "number" }
                                prop:value=inp
                                on:input=move |ev| inp.set(event_target_value(&ev)) />
                            <button class="btn" on:click=move |_| {
                                let v = inp.get_untracked();
                                // 不支持移除：清空时不写入，保留原值。
                                if v.is_empty() {
                                    return;
                                }
                                let value = match vt.as_str() {
                                    "integer" => v.parse::<i64>().ok().map(|i| Value::Number(i.into())),
                                    "float" => v
                                        .parse::<f64>()
                                        .ok()
                                        .and_then(serde_json::Number::from_f64)
                                        .map(Value::Number),
                                    _ => Some(Value::String(v)),
                                };
                                if let Some(val) = value {
                                    on_set.run(val);
                                }
                            }>{move || i18n.t("label.set")}</button>
                        </div>
                    }.into_any()
                }
            }}
            // 退出由外层 EntryTable 的全局 mousedown 监听负责：左键落在
            // `.lblcell.editing` 之外就把 editing_cell 清掉。这条路径不再
            // 需要「↩ 返回」按钮；亦无删除入口——视图表格不允许移除标签。
            {move || local_err.get().map(|e| view! { <div class="hint">{"⚠ "}{e}</div> })}
        </div>
    }
}

/// 右侧详情面板的四个页签（key 是稳定的）。
fn detail_tab_labels(i18n: &I18nController) -> [(String, String); 4] {
    [
        ("detail".to_string(), i18n.t("entry.tab_detail")),
        ("relations".to_string(), i18n.t("entry.tab_relations")),
        ("attachments".to_string(), i18n.t("entry.tab_attachments")),
        ("history".to_string(), i18n.t("entry.tab_history")),
    ]
}

/// 右侧详情面板：标题、详情与标签都可编辑，保存走乐观并发。
#[component]
fn EntryPanel(
    code: RwSignal<String>,
    slug: String,
    workspace_id: Signal<String>,
    schemas: RwSignal<Vec<LabelSchema>>,
    members: RwSignal<Vec<Member>>,
    refresh: RwSignal<u32>,
) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    let navigate = use_navigate();
    let data: RwSignal<Option<Result<Entry, String>>> = RwSignal::new(None);
    let labels = RwSignal::new(Vec::<Labeling>::new());
    let title = RwSignal::new(String::new());
    let detail = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    // `code` 在多处的 `view!` 里通过 `Signal::derive(move || code.get())` 传给子组件；
    // 父级在响应式闭包里反复创建这个 derive，Leptos 把它当作"新 prop"，
    // 子组件里的 TinyEditor（评论输入框）被卸掉重挂，粘贴中的图片就丢。
    // 这里提前物化一个稳定的 derived signal，让 prop 身份稳定。
    let code_signal: Signal<String> = Signal::derive(move || code.get());
    // 标题平时只读，点击才换成输入框；输入框挂载后由 Effect 补焦点。
    let editing_title = RwSignal::new(false);
    let title_ref: NodeRef<Input> = NodeRef::new();
    // 与全屏页同源：`data` 里的条目是服务端最新版本，无差异 ⇔ 没改动。
    // 标题比较必须带 trim：服务端存/返回前会 trim（service/entry.rs），若不加，
    // 用户输入 "abc " 存回 "abc"，两者永不相等，dirty 卡在 true、按钮再也不置灰。
    // 别「简化」掉这个 trim，也别改成回头 set 缓冲——那会动到用户正在打的字。
    let dirty = Signal::derive(move || {
        data.get()
            .and_then(|r| r.ok())
            .is_some_and(|e| e.title != title.get().trim() || e.detail != detail.get())
    });

    // 当前页签：detail | attachments | history。
    let tab = RwSignal::new("detail".to_string());
    // 工作空间审计日志，进来时取一次；「历史」页签按当前 code 过滤。
    let logs = RwSignal::new(Vec::<AuditLog>::new());

    // overwrite=true：清空编辑缓冲后全量填充（选中变化 / 并发冲突重载）。
    // overwrite=false：软重载，仅更新 data/labels，保留未保存的详情编辑。
    let load = move |overwrite: bool| {
        // 诊断日志：观察侧栏 EntryPanel 何时触发 load。
        #[cfg(target_arch = "wasm32")]
        leptos::web_sys::console::log_1(
            &format!("[EntryPanel] load(overwrite={})", overwrite).into(),
        );
        let c = code.get();
        if c.is_empty() {
            return;
        }
        if overwrite {
            title.set(String::new());
            editing_title.set(false);
            detail.set(String::new());
            labels.set(Vec::new());
            data.set(None);
            tab.set("detail".to_string());
            logs.set(Vec::new());
            error.set(None);
        }
        let ws = workspace_id.get_untracked();
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let result = entry(&c).await;
                // 审计日志取不到不该让整个面板失败：历史是附加信息，退化成一条记录都没有。
                let audit = if ws.is_empty() {
                    Vec::new()
                } else {
                    audit_logs(&ws).await.unwrap_or_default()
                };
                if code.get() != c {
                    return;
                }
                logs.set(audit);
                match result {
                    Ok(Some(e)) => {
                        labels.set(e.labels.clone());
                        if overwrite {
                            title.set(e.title.clone());
                            detail.set(e.detail.clone());
                        }
                        data.set(Some(Ok(e)));
                    }
                    Ok(None) => data.set(Some(Err(i18n.t("entry.errors.entry_not_found")))),
                    Err(err) => data.set(Some(Err(err))),
                }
            });
        }
    };

    Effect::new_sync(move |_| load(true));

    // 标签值改动后除了刷新面板自身，还必须触发列表重查：否则表格里的标签列
    // 会一直停在改动前的值（无值为「—」），直到整页刷新才更新。
    let on_changed = Callback::new(move |_| {
        load(false);
        refresh.update(|n| *n += 1);
    });
    // 附件上传只让面板自己重取条目（`updated_at` 推前了，乐观并发版本号失效）。
    // 不刷表：表格里不展示附件，刚才那张图也不会影响列表里任何一列。
    // 关键：必须与 `on_changed` 分离——后者会动 `refresh` 信号，触发整张表重查、
    // 父布局随之重建，连带把评论输入框里的 TinyEditor 整个换掉，
    // 用户刚粘贴还在上传的图就丢了。详见 `comment_list.rs` 的 `items_sig` 注释。
    let on_uploaded = Callback::new(move |_| {
        load(false);
    });
    let on_editor_change = Callback::new(move |d: String| detail.set(d));

    // AI 改了当前条目上的标签（labeling 域）→ 面板里那一份 `labels` 也得跟着刷。
    // 仅当 `code` 非空（面板已挂载）时才有意义，空码时 load 会早返；`load(false)`
    // 保留未保存的详情编辑。
    if let Some(eff) = use_context::<crate::frontend::agent_side_effects::AgentSideEffects>() {
        Effect::new(move |_| {
            eff.tick.track();
            let Some(hint) = eff.last.get_untracked() else {
                return;
            };
            if hint.domain == "labeling" {
                load(false);
            }
        });
    }

    // 进入标题编辑态后把焦点交给输入框，否则用户还得再点一次。
    #[cfg(target_arch = "wasm32")]
    Effect::new(move |_| {
        if editing_title.get() {
            if let Some(el) = title_ref.get() {
                let _ = el.focus();
            }
        }
    });

    let do_save: Callback<()> = Callback::new(move |_| {
        // 无改动就不写：服务端即便 title/detail 未变也会 bump updated_at 并追加一条审计，
        // 而 updated_at 是乐观并发比的令牌。按钮那边由 disabled 挡住，快捷键这条路径得自己挡。
        if !dirty.get() {
            return;
        }
        let c = code.get();
        let Some(entry_now) = data.get().and_then(|r| r.ok()) else {
            return;
        };
        let expected = entry_now.updated_at.clone();
        let t = title.get();
        let d = detail.get();
        spawn_local(async move {
            match update_entry(&c, &expected, &t, &d).await {
                Ok(updated) => {
                    labels.set(updated.labels.clone());
                    data.set(Some(Ok(updated)));
                    error.set(None);
                    refresh.update(|n| *n += 1);
                }
                Err(e) => {
                    let concurrent = i18n.t("entry.errors.concurrent_edit");
                    error.set(Some(e.clone()));
                    if e.contains(&concurrent) {
                        load(true);
                    }
                }
            }
        });
    });

    let del = move |_| {
        let c = code.get();
        spawn_local(async move {
            match delete_entry(&c).await {
                Ok(true) => {
                    code.set(String::new());
                    refresh.update(|n| *n += 1);
                }
                Ok(false) => error.set(Some(i18n.t("entry.errors.delete_failed"))),
                Err(e) => error.set(Some(e)),
            }
        });
    };

    // 关闭动作：面板的 `×` 按钮与窗口级 Esc 监听两处共用。
    let close: Callback<()> = Callback::new(move |_| code.set(String::new()));

    // Esc 关右侧面板；有未保存改动时拦住。
    // 面板是无条件挂载的（显隐靠 code 是否为空判断），所以这里仍是常驻的窗口级监听。
    if cfg!(target_arch = "wasm32") {
        let handle = window_event_listener(leptos::ev::keydown, move |ev| {
            if ev.key() != "Escape" || code.get_untracked().is_empty() {
                return;
            }
            if dirty.get_untracked() {
                error.set(Some(i18n.t("entry.errors.unsaved_block")));
                return;
            }
            close.run(());
        });
        on_cleanup(move || handle.remove());
    }

    // 单品归档：面板只可能显示未归档的条目（归档条目已被列表过滤掉，取消归档走
    // 「已归档」弹窗），故这里是单向操作，归档成功后条目移出视图、面板关闭。
    let arch = move |_| {
        let c = code.get();
        spawn_local(async move {
            match archive_entry(&c).await {
                Ok(true) => {
                    code.set(String::new());
                    refresh.update(|n| *n += 1);
                }
                Ok(false) => error.set(Some(i18n.t("entry.errors.archive_failed"))),
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let open_full = move |_| {
        let c = code.get();
        if !c.is_empty() {
            navigate(&format!("/{}/entry/{}", slug, c), Default::default());
        }
    };

    view! {
        {move || {
            if code.get().is_empty() {
                view! { <div></div> }.into_any()
            } else {
                view! {
                    <aside class="detail">
                        {move || error.get().map(|e| view! { <div class="hint">{"⚠ "}{e}</div> })}
                        <div class="dhead">
                            <div class="dacts">
                                <button class="btn sm" on:click=open_full.clone()>{ic_full()}{move || i18n.t("entry.fullscreen")}</button>
                                <button class="btn pri sm" disabled=move || !dirty.get() on:click=move |_| do_save.run(())>{move || i18n.t("common.save")}</button>
                                <button class="btn sm" on:click=arch>{move || i18n.t("entry.archive")}</button>
                                <button class="btn danger sm" on:click=del>{move || i18n.t("common.delete")}</button>
                                <button class="ibtn" title=move || i18n.t("entry.close_panel") on:click=move |_| close.run(())>{ic_close()}</button>
                            </div>
                            {move || if editing_title.get() {
                                view! {
                                    <input class="inp dtitle"
                                        node_ref=title_ref
                                        prop:value=title
                                        on:input=move |ev| title.set(event_target_value(&ev))
                                        on:blur=move |_| editing_title.set(false)
                                        on:keydown=move |ev| {
                                            if ev.key() == "Enter" {
                                                ev.prevent_default();
                                                editing_title.set(false);
                                            } else if (ev.ctrl_key() || ev.meta_key()) && ev.key().eq_ignore_ascii_case("s") {
                                                ev.prevent_default();
                                                do_save.run(());
                                            }
                                        }
                                    />
                                }.into_any()
                            } else {
                                view! {
                                    <h3 class="dtitle"
                                        title=move || i18n.t("entry.edit_title_hint")
                                        on:click=move |_| editing_title.set(true)
                                    >{move || title.get()}</h3>
                                }.into_any()
                            }}
                            {move || {
                                let c = code.get();
                                if c.is_empty() {
                                    view! { <div></div> }.into_any()
                                } else {
                                    view! { <CodeCopy code=Signal::derive(move || c.clone()) /> }.into_any()
                                }
                            }}
                        </div>
                        {move || data.get().and_then(|r| r.ok()).map(|e| {
                            let by = |a: &Option<AccountBrief>| a.as_ref().map(|x| x.name.clone()).unwrap_or_else(|| "—".to_string());
                            view! {
                                <div class="dmeta">
                                    <div><span class="mut">{move || i18n.t("entry.created_by")}</span>{by(&e.created_by_account)}</div>
                                    <div><span class="mut">{move || i18n.t("entry.created_at")}</span>{fmt_datetime(&e.created_at)}</div>
                                    <div><span class="mut">{move || i18n.t("entry.updated_by")}</span>{by(&e.updated_by_account)}</div>
                                    <div><span class="mut">{move || i18n.t("entry.updated_at")}</span>{fmt_datetime(&e.updated_at)}</div>
                                    {e.archived_at.clone().map(|at| view! {
                                        <div><span class="mut">{move || i18n.t("entry.archived_at")}</span>{fmt_datetime(&at)}</div>
                                    })}
                                </div>
                            }
                        })}
                        <div class="editing"><span class="dot"></span>{move || i18n.t("entry.optimistic_lock_hint")}</div>
                        <TabBar tabs=detail_tab_labels(&i18n).to_vec() active=tab />
                        {move || match tab.get().as_str() {
                            "relations" => view! {
                                <RelationsPanel
                                    code=code_signal
                                    workspace_id=workspace_id
                                    on_changed=on_changed
                                />
                            }.into_any(),
                            "attachments" => view! {
                                <AttachmentList
                                    code=code_signal
                                    workspace_id=workspace_id
                                    on_changed=on_changed
                                />
                            }.into_any(),
                            "history" => view! {
                                <AuditTimeline
                                    logs=logs
                                    code=code_signal
                                />
                            }.into_any(),
                            _ => view! {
                                <div class="editor"
                                    on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                                        if (ev.ctrl_key() || ev.meta_key()) && ev.key().eq_ignore_ascii_case("s") {
                                            ev.prevent_default();
                                            do_save.run(());
                                        }
                                    }>
                                    {match data.get() {
                                        Some(Ok(e)) => {
                                            let initial = e.detail.clone();
                                            // 同 entry.rs 注释：get_untracked 避免
                                            // 成员表后续变化把编辑器重挂。
                                            let ms = members.get_untracked();
                                            view! {
                                                <TinyEditor initial entry_code=code_signal
                                                    on_change=on_editor_change on_uploaded=on_uploaded
                                                    members=ms />
                                            }.into_any()
                                        }
                                        _ => view! {
                                            <div class="ebody"><span class="mut">{move || i18n.t("common.loading")}</span></div>
                                        }.into_any(),
                                    }}
                                </div>
                                <LabelEditor code=code schemas labels members on_changed />
                            }.into_any(),
                        }}
                        // 评论列表放在 tab 之外：之前塞在 `_ =>` 分支的 view! 里时，
                        // detail tab 的编辑器重渲染（`match data.get()` 重新求值）让外层
                        // view! 整体被重算，Leptos diff 在这个边角下把 CommentList 槽位
                        // 当作新组件，导致评论输入框（也是 TinyEditor）整块被卸载重挂，
                        // 用户刚粘贴还在上传的图就丢了。提到 tab 之外后，无论用户在哪个
                        // tab，CommentList 只挂一次。
                        // 视觉上让评论常驻面板底部，与全屏详情页（entry.rs）一致。
                        <CommentList
                            code=code_signal
                            workspace_id=workspace_id
                            on_changed=on_changed
                            on_uploaded=on_uploaded
                        />
                    </aside>
                }
                .into_any()
            }
        }}
    }
}

/// 「新建 Entry」表单里的标签行：沿用 `LabelEditor` 的 chip 外观，但只写本地草稿，
/// 等条目创建成功后再由 `create_submit` 逐个 upsert。
#[component]
fn LabelDraft(rows: RwSignal<Vec<DraftLabel>>, members: RwSignal<Vec<Member>>) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    view! {
        <div class="lbledit">
            {move || {
                rows.get()
                    .into_iter()
                    .map(|r| {
                        let title = if r.title.trim().is_empty() { r.name.clone() } else { r.title.clone() };
                        if r.value_type == "null" {
                            // 无值标签只有「打上 / 不打」两种状态。
                            let flag = r.flag;
                            let chip = title.clone();
                            view! {
                                <div class="lblrow">
                                    <span class="k">{ic_tag()}{title}</span>
                                    <label class="mut" style="display:flex;align-items:center;gap:4px">
                                        <input type="checkbox"
                                            prop:checked=move || flag.get() == Some(true)
                                            on:change=move |ev| flag.set(event_target_checked(&ev).then_some(true)) />
                                        <span>{move || if flag.get() == Some(true) { chip.clone() } else { i18n.t("label.apply") }}</span>
                                    </label>
                                </div>
                            }.into_any()
                        } else if r.value_type == "enum" && r.multi {
                            // 多选 Enum：勾选集合整体写成一个数组值。
                            let opts = r.enum_values.get();
                            let many = r.many;
                            view! {
                                <div class="lblrow">
                                    <span class="k">{ic_tag()}{title}</span>
                                    <div class="multienum">
                                        {opts.into_iter().map(|o| {
                                            let oc_chk = o.clone();
                                            let oc_set = o.clone();
                                            let hit = o.clone();
                                            view! {
                                                <label class="mut">
                                                    <input type="checkbox"
                                                        prop:checked=move || many.get().contains(&oc_chk)
                                                        on:change=move |ev| {
                                                            if event_target_checked(&ev) {
                                                                let oc = oc_set.clone();
                                                                many.update(|v| if !v.contains(&oc) { v.push(oc.clone()) });
                                                            } else {
                                                                let oc = oc_set.clone();
                                                                many.update(|v| v.retain(|x| x != &oc));
                                                            }
                                                        } />
                                                    {display_enum_value(&hit)}
                                                </label>
                                            }
                                        }).collect::<Vec<_>>()}
                                    </div>
                                </div>
                            }.into_any()
                        } else if r.value_type == "enum" {
                            let opts = r.enum_values.get();
                            let sel = r.text;
                            view! {
                                <div class="lblrow">
                                    <span class="k">{ic_tag()}{title}</span>
                                    <select on:change=move |ev| sel.set(event_target_value(&ev))>
                                        <option value="" selected=move || sel.get().is_empty()>{move || i18n.t("common.unset")}</option>
                                        {opts.into_iter().map(|o| {
                                            let oc = o.clone();
                                            let hit = o.clone();
                                            view! {
                                                <option value=oc.clone() selected=move || sel.get() == oc>
                                                    {display_enum_value(&hit)}
                                                </option>
                                            }
                                        }).collect::<Vec<_>>()}
                                    </select>
                                </div>
                            }.into_any()
                        } else if r.value_type == "boolean" {
                            let flag = r.flag;
                            view! {
                                <div class="lblrow">
                                    <span class="k">{ic_tag()}{title}</span>
                                    <select on:change=move |ev| {
                                        flag.set(match event_target_value(&ev).as_str() {
                                            "true" => Some(true),
                                            "false" => Some(false),
                                            _ => None,
                                        });
                                    }>
                                        <option value="" selected=move || flag.get().is_none()>{move || i18n.t("common.unset")}</option>
                                        <option value="true" selected=move || flag.get() == Some(true)>{move || i18n.t("common.yes")}</option>
                                        <option value="false" selected=move || flag.get() == Some(false)>{move || i18n.t("common.no")}</option>
                                    </select>
                                </div>
                            }.into_any()
                        } else if r.value_type == "account" && r.multi {
                            // 多选 Account：勾选集合整体写成一个数组值，与多选 Enum 同款。
                            // 之前 `to_value` 从 `self.many` 读、`LabelDraft` 却写到
                            // `self.text`（逗号分隔串），提交时空集被当作「未设置」而整条
                            // 标签被跳过——账号多选因此静默失败。
                            let sel = r.many;
                            let picked = Callback::new(move |ids: Vec<String>| {
                                sel.set(ids);
                            });
                            // `sel` 是 RwSignal<Vec<String>>，AccountPicker 按 Signal
                            // 读：内部 pick 闭包每次 get_untracked 拿最新值，多选不会
                            // 把之前的勾选「覆盖」掉；chip 渲染随 sel 变化刷新。
                            view! {
                                <div class="lblrow">
                                    <span class="k">{ic_tag()}{title}</span>
                                    <AccountPicker members=members.get() selected=sel
                                        on_change=picked />
                                </div>
                            }.into_any()
                        } else if r.value_type == "account" {
                            let sel = r.text;
                            // 单选 Account：新建 Entry 草稿没有原值可保留——
                            // 清空选择时 picked 写入空串，to_value 把空串视为「本次不写」，
                            // 行为与同文件 `label_row`（编辑既有标签）一致，
                            // 与 `LabelEditor::LabelRow` 的「清空即移除」语义不同。
                            let picked = Callback::new(move |ids: Vec<String>| {
                                sel.set(ids.into_iter().next().unwrap_or_default());
                            });
                            // AccountPicker 现在按 Signal<Vec<String>> 读 selected；
                            // 把 sel.get() 的派生包进 Signal::derive，sel 变化时
                            // chip 立刻刷新（单选场景不再卡在「未设置」）。
                            let selected = Signal::derive(move || {
                                let v = sel.get();
                                if v.is_empty() { Vec::new() } else { vec![v] }
                            });
                            view! {
                                <div class="lblrow">
                                    <span class="k">{ic_tag()}{title}</span>
                                    <AccountPicker members=members.get() selected=selected
                                        on_change=picked />
                                </div>
                            }.into_any()
                        } else {
                            let inp = r.text;
                            // 时间类默认布局走原生控件；自定义 Go 布局原生控件表达不了，
                            // 退回文本输入（与 LabelRow 一致）。金额 / 整数 / 浮点走数字
                            // 输入；其余为文本。
                            let is_time = matches!(r.value_type, "date" | "time" | "datetime");
                            let custom_layout =
                                is_time && !is_native_time_layout(r.value_type, r.format.as_deref());
                            let input_type = if custom_layout {
                                "text"
                            } else {
                                match r.value_type {
                                    "string" => "text",
                                    "email" => "email",
                                    "date" => "date",
                                    "time" => "time",
                                    "datetime" => "datetime-local",
                                    _ => "number",
                                }
                            };
                            view! {
                                <div class="lblrow">
                                    <span class="k">{ic_tag()}{title}</span>
                                    <input class="inp" type=input_type prop:value=inp
                                        on:input=move |ev| inp.set(event_target_value(&ev)) />
                                </div>
                            }.into_any()
                        }
                    })
                    .collect::<Vec<_>>()
                    .into_any()
            }}
        </div>
    }
}

/// 列选择器：把工作空间的标签以可勾选 chip 列出，产出「展示为列」的标签 name 列表。
/// 用选择代替自由文本输入——用户手写的展示名与服务端要求的 name 不一致会被拒绝，
/// 让新建/配置视图在无声中失败。
///
/// 顶部先摆 4 枚元信息 chip（创建人 / 创建时间 / 更新人 / 更新时间），再摆工作空间的
/// 全部标签 chip。元信息与标签语义不同，混排会让勾选状态难追——两类各成一段。
#[component]
fn ColumnPicker(
    schemas: RwSignal<Vec<LabelSchema>>,
    selected: RwSignal<Vec<String>>,
) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    view! {
        <div class="colpick">
            {metadata_columns(&i18n).into_iter().map(|(col, _field, label)| {
                // 三个闭包（class / checked / on:change）各自搬一份 `col` 字符串。
                // 三个闭包都要查 `selected` 里有没有这个 sentinel；原值进 update 闭包后
                // 还要再被 position/push 用一次，所以 on:change 那一份还得再 clone 一次。
                let col_for_class = col.to_string();
                let col_for_check = col.to_string();
                let col_for_change = col.to_string();
                let col_for_change_clone = col.to_string();
                view! {
                    <label class=move || {
                        if selected.get().iter().any(|n| n == &col_for_class) { "colchip on" } else { "colchip" }
                    }>
                        <input
                            type="checkbox"
                            prop:checked=move || selected.get().iter().any(|n| n == &col_for_check)
                            on:change=move |_| {
                                let me = col_for_change_clone.clone();
                                selected.update(|v| {
                                    if let Some(i) = v.iter().position(|x| *x == me) {
                                        v.remove(i);
                                    } else {
                                        v.push(col_for_change.clone());
                                    }
                                });
                            }
                        />
                        {label}
                    </label>
                }
            }).collect::<Vec<_>>()}
            {move || {
                let list = schemas.get();
                if list.is_empty() {
                    return view! { <span class="mut">{move || i18n.t("entry.no_labels")}</span> }.into_any();
                }
                list.into_iter()
                    .map(|s| {
                        let title = if s.title.trim().is_empty() {
                            s.name.clone()
                        } else {
                            s.title.clone()
                        };
                        let name = s.name.clone();
                        let name_cls = name.clone();
                        let name_chk = name.clone();
                        view! {
                            <label class=move || {
                                if selected.get().contains(&name_cls) { "colchip on" } else { "colchip" }
                            }>
                                <input
                                    type="checkbox"
                                    prop:checked=move || selected.get().contains(&name_chk)
                                    on:change=move |_| {
                                        selected
                                            .update(|v| {
                                                if let Some(i) = v.iter().position(|x| x == &name) {
                                                    v.remove(i);
                                                } else {
                                                    v.push(name.clone());
                                                }
                                            });
                                    }
                                />
                                {title}
                            </label>
                        }
                    })
                    .collect::<Vec<_>>()
                    .into_any()
            }}
        </div>
    }
}

/// 排序优先级角标。超过 9 个键时退回 `(n)`，实际用不到那么深。
const PRIO_MARKS: [&str; 9] = ["①", "②", "③", "④", "⑤", "⑥", "⑦", "⑧", "⑨"];

/// 第 `i` 位的优先级角标；超过 9 个键时退回 `(n)`。
fn prio_mark(i: usize) -> String {
    PRIO_MARKS
        .get(i)
        .map(|m| m.to_string())
        .unwrap_or_else(|| format!("({})", i + 1))
}

/// 表头上的排序标记：链上位置给 `①/②/…`，方向给 `↑/↓`；不在链上则空串。
fn sort_mark(sorts: &[ViewSort], field: &str) -> String {
    match sorts.iter().position(|s| s.field == field) {
        Some(i) => {
            let mark = prio_mark(i);
            format!(" {mark} {}", if sorts[i].desc { "↓" } else { "↑" })
        }
        None => String::new(),
    }
}

/// 表头点击的参数：`additive` 来自 `Shift` 键（追加为次级排序键），
/// `remove` 来自双击同一字段（从排序链里剔除）。
#[derive(Clone)]
struct SortRequest {
    field: String,
    additive: bool,
    /// 双击触发：把字段从排序链里拿掉。
    remove: bool,
}

/// 时间型内置名 / 标签的值，用哪种原生控件表达。
#[derive(Clone, Copy, PartialEq)]
enum TimeKind {
    Date,
    Time,
    DateTime,
}

/// 光标前的最后一段若形如「键 + 比较运算符 + 结尾空白」（运算符后尚未写值），
/// 且键属于时间型标签或 CreatedAt / UpdatedAt，返回 (类型, 插入位置)。
/// 纯手写扫描，不引入 regex。键与运算符之间允许空白。
fn detect_time_picker(
    text: &str,
    kind_of: &dyn Fn(&str) -> Option<TimeKind>,
) -> Option<(TimeKind, usize)> {
    let trimmed = text.trim_end();
    if trimmed.len() == text.len() {
        return None; // 运算符后必须有空白，才说明「值还没写」
    }
    let op_len = if trimmed.ends_with(">=") || trimmed.ends_with("<=") || trimmed.ends_with("!=") {
        2
    } else if trimmed.ends_with('>') || trimmed.ends_with('<') || trimmed.ends_with('=') {
        1
    } else {
        return None;
    };
    let mut key_end = trimmed.len() - op_len;
    // 键与运算符之间允许空白（用户示例就是 `CreateTime >`）。
    while key_end > 0 {
        let c = trimmed[..key_end].chars().next_back()?;
        if c.is_whitespace() {
            key_end -= c.len_utf8();
        } else {
            break;
        }
    }
    // 从运算符左侧往回扫键。逐字符后退（按 `len_utf8`），保证切片始终落在
    // 字符边界上——键可能是中文（`is_alphanumeric` 对 CJK 为真）。
    let mut j = key_end;
    while j > 0 {
        let c = trimmed[..j].chars().next_back()?;
        if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' {
            j -= c.len_utf8();
        } else {
            break;
        }
    }
    if j == key_end {
        return None;
    }
    let key = &trimmed[j..key_end];
    kind_of(key).map(|k| (k, text.len()))
}

/// 取出表达式里最后一个 `/` 之后的输入片段——即「正在输入的标签名」。
/// 无 `/` 时返回 `None`（不弹候选）。`/` 也可能出现在引号内的字符串里，
/// 但表达式语法中标签名只出现在裸词位置，这里按最后一次出现处理足够准确。
fn label_fragment(text: &str) -> Option<&str> {
    text.rfind('/').map(|i| &text[i + 1..])
}
