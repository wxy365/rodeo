use leptos::ev::SubmitEvent;
use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};

use crate::frontend::attachment_list::AttachmentList;
use crate::frontend::comment_list::CommentList;
use crate::frontend::components::{
    display_enum_value, fmt_datetime, from_native, is_native_time_layout, label_chip_class,
    logged_out, member_label, short_time, to_native, value_to_string, AccountPicker, AuditTimeline,
    CodeCopy, ColorPick, TabBar,
};
use crate::frontend::graphql_client::{
    archive_entry, archived_entries, audit_logs, create_entry, create_view, delete_entry,
    delete_view, entry, format_view_query, get_sidebar_collapsed, label_schemas, members,
    parse_view_query, query_all_entries, query_entries, set_labeling, set_labelings,
    set_sidebar_collapsed, set_view_timeline, summarize_entries, unarchive_entry, update_entry,
    update_view, views, workspace_ai_config, workspace_by_slug, AccountBrief, AuditLog, Entry,
    Labeling, LabelSchema, Member, NamedPrompt, View, ViewSort, ViewTimeline, Workspace,
};
use crate::frontend::icons::{
    ic_add, ic_back, ic_close, ic_comment, ic_folder, ic_full, ic_help, ic_history, ic_search,
    ic_setting, ic_share, ic_tag,
};
use crate::frontend::label_editor::LabelEditor;
use crate::frontend::query_eval;
use crate::frontend::timeline::TimelineView;
use crate::frontend::tiny_editor::TinyEditor;
use crate::frontend::use_auth;
use crate::frontend::view_filter::with_text;
use serde_json::Value;

/// 内置元数据关键字 → 中文展示名。恒定并入 `/` 候选，与视图里有哪些标签无关
/// （`src/domain/query.rs` 的 `RESERVED_FIELDS` 是同一份名单）。
const BUILTIN_FIELDS: [(&str, &str); 7] = [
    ("Code", "编码"),
    ("Title", "标题"),
    ("Detail", "详情"),
    ("CreatedBy", "创建人"),
    ("CreatedAt", "创建时间"),
    ("UpdatedBy", "更新人"),
    ("UpdatedAt", "更新时间"),
];

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
            "null" => self.flag.get_untracked().filter(|b| *b).map(|_| Value::Null),
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
            "time" | "datetime" if is_native_time_layout(self.value_type, self.format.as_deref()) => {
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
    let slug = move || params.get().get("slug").unwrap_or_default();
    let navigate = use_navigate();
    let auth = use_auth();
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
    let selected = RwSignal::new(String::new());
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
    let show_batch = RwSignal::new(false);
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

    // ---- 筛选查询状态 ----
    let query_ast = RwSignal::new(serde_json::json!({ "and": [] }));
    // 已落库的视图查询条件基线。排序改动会自动落库（见 `persist_sort`），所以不再进基线——
    // 否则每点一次表头，「保存视图」按钮都会无意义地亮起来。
    // 基础视图没有这个按钮（它上面的过滤只作临时用途），但基线仍要维护——「视图配置」
    // 保存后会用它重新对齐。
    let saved_baseline: RwSignal<Value> = RwSignal::new(serde_json::json!({ "and": [] }));
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
    let load_views = move |ws_id: String| {
        spawn_local(async move {
            if let Ok(list) = views(&ws_id).await {
                // 仅当当前选中视图不在新列表里（切换工作空间、或列表为空）才重设，
                // 否则会把用户在侧栏上的选择覆盖回第一个视图。
                let current = active_id.get_untracked();
                let still_valid = current
                    .as_ref()
                    .is_some_and(|id| list.iter().any(|v| &v.id == id));
                if !still_valid {
                    set_active(list.first().cloned());
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

    // 把表达式输入框的内容解析成查询 AST 并应用（回车触发）。空输入即清空过滤。
    // 解析交给服务端 `parse_view_query`：语法与标签 schema 校验只有一份实现。
    let apply_expr = move || {
        let expr = expr_text.get_untracked();
        let Some(ws_id) = data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.id.clone()) else {
            error.set(Some("工作空间尚未加载完成".to_string()));
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

    // 已落库的查询条件与当前编辑态有差异即视为有未保存改动。
    let view_dirty = move || {
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
                match update_view(&v.id, &v.name, &v.query, &v.sorts, &v.columns, v.is_shared, &v.title_colors).await {
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
                    Err(e) => error.set(Some(format!("排序未能保存：{e}"))),
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
                    query_entries(&ws.id, &ast, &sorts, page_now, page_size).await?
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
                    load_views(w.id.clone());
                    data.set(Some(Ok((w, items, list))));
                }
                Err(e) => data.set(Some(Err(e))),
            }
        });
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
            match create_entry(&id, &t).await {
                Err(e) => error.set(Some(e)),
                Ok(created) => {
                    // 服务端 createEntry 只接收标题，标签需在条目存在后逐个 upsert。
                    // 单个标签失败不丢弃已创建的条目，但要如实报出来。
                    for (name, value) in &planned {
                        if let Err(e) = set_labeling(&created.code, name, value).await {
                            error.set(Some(format!("条目已创建，但标签「{name}」写入失败：{e}")));
                            break;
                        }
                    }
                    new_title.set(String::new());
                    new_labels.set(Vec::new());
                    show_new.set(false);
                    refresh.update(|n| *n += 1);
                }
            }
        });
    };

    // 双击行 / AI 新建后进入条目全屏页，与详情面板的「全屏」按钮走同一条路由。
    let nav_entry = use_navigate();
    let open_entry = Callback::new(move |code: String| {
        if !code.is_empty() {
            nav_entry(&format!("/{}/entry/{}", slug(), code), Default::default());
        }
    });

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
            let is_text_input = target.as_ref().and_then(|t| {
                t.dyn_ref::<leptos::web_sys::HtmlInputElement>().map(|el| {
                    let t = el.type_();
                    t == "text" || t == "search" || t == "email" || t == "password" || t == "url"
                })
            }).unwrap_or(false);
            let is_textarea = target
                .as_ref()
                .and_then(|t| t.dyn_ref::<leptos::web_sys::HtmlTextAreaElement>().map(|_| true))
                .unwrap_or(false);
            let is_select = target
                .as_ref()
                .and_then(|t| t.dyn_ref::<leptos::web_sys::HtmlSelectElement>().map(|_| true))
                .unwrap_or(false);
            let is_ce = target
                .as_ref()
                .and_then(|t| t.dyn_ref::<leptos::web_sys::HtmlElement>().map(|el| el.is_content_editable()))
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
                    if i + 1 < codes.len() { i + 1 } else {
                        // 已经在末位：尝试翻到下一页首位。
                        let total = total_signal.get_untracked();
                        let pages = ((total + page_size - 1) / page_size).max(1);
                        let cur_page = page_signal.get_untracked();
                        if cur_page < pages {
                            page_signal.set(cur_page + 1);
                            0
                        } else { i }
                    }
                }
                (Some(i), "ArrowUp") => {
                    if i > 0 { i - 1 } else {
                        let cur_page = page_signal.get_untracked();
                        if cur_page > 1 {
                            page_signal.set(cur_page - 1);
                            // 翻页前 items 仍是旧页数据，等新页加载后会重新触发该判断。
                            // 这里直接选 0 也无妨：未翻页前就是 0。
                            0
                        } else { i }
                    }
                }
                // 表格里没有任何选中条目：从首 / 末位开始，给键盘一个落脚点。
                (None, "ArrowDown") => 0,
                (None, "ArrowUp") => codes.len() - 1,
                _ => return,
            };
            if let Some(code) = codes.get(new_idx) {
                selected.set(code.clone());
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
            batch_error.set(Some("请至少填写一个标签值".to_string()));
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
        let Some(ws_id) = data.get_untracked().and_then(|r| r.ok()).map(|(w, _, _)| w.id) else {
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
        let Some(ws_id) = data.get_untracked().and_then(|r| r.ok()).map(|(w, _, _)| w.id) else {
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
        let Some(ws_id) = data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.id.clone()) else {
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
                        failure = Some("归档失败".to_string());
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
                error.set(Some(format!("已归档 {done} 个，其余失败：{e}")));
            }
        });
    };

    view! {
        <div class="page page-app">
            <div class="crumb">
                {move || match active_view.get() {
                    Some(v) => format!("/{} · 视图「{}」", slug(), v.name),
                    None => format!("/{} · 基础视图「全部内容」", slug()),
                }}
            </div>
            <div class=move || if sidebar_collapsed.get() { "ws-layout collapsed" } else { "ws-layout" }>
                <WorkspaceSidebar
                    slug=slug().to_string()
                    name=ws_name
                    views=view_list
                    active=active_id
                    collapsed=sidebar_collapsed
                    on_select=select_view
                    on_new=Callback::new(move |_| {
                        batch(move || {
                            view_dialog_saveas.set(false);
                            view_query_input.set(serde_json::json!({ "and": [] }));
                            view_sort_input.set(Vec::new());
                            view_name_input.set(String::new());
                            view_columns_input.set(Vec::new());
                            view_shared_input.set(false);
                            dialog_error.set(None);
                            show_view_dialog.set(true);
                        });
                    })
                    on_delete=delete_view_cb
                    on_archived=Callback::new(open_archived)
                />
                <div class="panel wmain">
                    <div class="vhead">
                        <h2>{move || active_view.get().map(|v| v.name).unwrap_or_else(|| "全部内容".to_string())}</h2>
                        <label class="inp">
                            {ic_search()}
                            <input placeholder="搜索本视图，可与过滤组合" prop:value=ad_hoc_text
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
                        {move || active_view.get().is_some_and(|v| v.timeline.is_some()).then(|| view! {
                            <div class="seg">
                                <button class=move || if timeline_mode.get() { "" } else { "on" }
                                    on:click=move |_| timeline_mode.set(false)>"普通视图"</button>
                                <button class=move || if timeline_mode.get() { "on" } else { "" }
                                    on:click=move |_| timeline_mode.set(true)>"时间轴"</button>
                            </div>
                        })}
                        <button class="btn" on:click=move |_| {
                            let Some(v) = active_view.get() else {
                                error.set(Some("请先选择或新建一个视图".to_string()));
                                return;
                            };
                            config_name_input.set(v.name.clone());
                            config_columns_input.set(v.columns.clone());
                            config_shared_input.set(v.is_shared);
                            config_tl_start.set(
                                v.timeline.as_ref().map(|t| t.start.clone()).unwrap_or_default(),
                            );
                            config_tl_end.set(
                                v.timeline.as_ref().map(|t| t.end.clone()).unwrap_or_default(),
                            );
                            config_tl_person.set(
                                v.timeline.as_ref().and_then(|t| t.person.clone()).unwrap_or_default(),
                            );
                            dialog_error.set(None);
                            // 预填标题颜色规则：先以缓存的 AST 建行，表达式文本异步反格式化回填。
                            let raw = v.title_colors.as_array().cloned().unwrap_or_default();
                            let rows: Vec<TitleRule> = raw.iter().enumerate().map(|(i, r)| {
                                TitleRule {
                                    id: i as u32,
                                    expr: RwSignal::new(String::new()),
                                    color: RwSignal::new(
                                        r.get("color").and_then(|c| c.as_str())
                                            .unwrap_or("#3b82f6").to_string(),
                                    ),
                                    cached_expr: RwSignal::new(String::new()),
                                    cached_ast: RwSignal::new(
                                        r.get("query").cloned().unwrap_or(Value::Null),
                                    ),
                                }
                            }).collect();
                            next_rule_id.set(rows.len() as u32);
                            config_rules.set(rows);
                            show_config_dialog.set(true);
                            let ws_id = data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.id.clone());
                            let ids: Vec<u32> = config_rules.get_untracked().iter().map(|r| r.id).collect();
                            if ids.is_empty() { return; }
                            let Some(ws_id) = ws_id else { return };
                            rules_loading.set(true);
                            spawn_local(async move {
                                for id in ids {
                                    let ast = config_rules.get_untracked().iter()
                                        .find(|r| r.id == id)
                                        .map(|r| r.cached_ast.get_untracked())
                                        .unwrap_or(Value::Null);
                                    if ast.is_null() { continue; }
                                    if let Ok(s) = format_view_query(&ws_id, &ast).await {
                                        config_rules.update(|rows| {
                                            if let Some(r) = rows.iter_mut().find(|r| r.id == id) {
                                                // 用户可能在请求返回前已开始输入，此时不覆盖。
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
                        }>{ic_setting()}"视图配置"</button>
                        <button class="btn pri" on:click=move |_| {
                            if !show_new.get_untracked() {
                                // 打开时按当前标签 schema 重建待填行（新建与取消都重置）。
                                // 视图列与视图表达式里用到的标签预填默认值：这两类标签
                                // 是当前视图关心的，新条目本来就该带着它们。
                                let auto = auto_label_names(
                                    active_view.get_untracked().map(|v| v.columns).unwrap_or_default(),
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
                        }>
                            {ic_add()}
                            "新建 Entry"
                        </button>
                    </div>
                    <div class="filters">
                        <div class="exprwrap">
                            <button class="exprhelp" title="标签表达式语法说明"
                                on:click=move |_| expr_help.set(true)>{ic_help()}</button>
                            <input class="inp" style="width:100%;padding-right:26px"
                                placeholder=r#"标签表达式：Task AND !Bug（回车应用；输入 / 选标签）"#
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
                                let mut items: Vec<(String, String)> = BUILTIN_FIELDS
                                    .iter()
                                    .map(|(k, t)| (k.to_string(), t.to_string()))
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
                        {move || if active_view.get().is_some_and(|v| v.is_default) {
                            // 基础视图上的过滤是临时态：不给「保存」，要留存只能另存为新视图。
                            view! {
                                <button class="btn" style="margin-left:auto"
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
                                    }>"另存为新视图"</button>
                                // 重置：把当前临时过滤与全文搜索输入复原到基础视图的初始态。
                                // 仅在确有改动时才点亮，避免空操作。
                                <button class="ibtn" title="重置为默认"
                                    disabled=move || !view_dirty()
                                    on:click=move |_| {
                                        query_ast.set(serde_json::json!({ "and": [] }));
                                        ad_hoc_text.set(String::new());
                                        expr_text.set(String::new());
                                        hint_open.set(false);
                                        time_pick.set(None);
                                        refresh_view.update(|n| *n += 1);
                                    }>{ic_history()}</button>
                            }.into_any()
                        } else {
                            view! {
                                <button class="btn" style="margin-left:auto" disabled=move || !view_dirty()
                                    on:click=move |_| {
                                        let Some(v) = active_view.get() else { return };
                                        let ast = query_ast.get();
                                        let sorts = v.sorts.clone();
                                        let cols = v.columns.clone();
                                        let shared = v.is_shared;
                                        let id = v.id.clone();
                                        let name = v.name.clone();
                                        let title_colors = v.title_colors.clone();
                                        spawn_local(async move {
                                            match update_view(&id, &name, &ast, &sorts, &cols, shared, &title_colors).await {
                                                Ok(saved) => {
                                                    view_list.update(|l| {
                                                        if let Some(slot) = l.iter_mut().find(|x| x.id == saved.id) {
                                                            *slot = saved.clone();
                                                        }
                                                    });
                                                    active_view.set(Some(saved));
                                                    saved_baseline.set(ast);
                                                    error.set(None);
                                                }
                                                Err(e) => error.set(Some(e)),
                                            }
                                        });
                                    }>"保存视图"</button>
                            }.into_any()
                        }}
                        <span class="mut">{move || {
                            let sorts = active_view.get().map(|v| v.sorts).unwrap_or_default();
                            if sorts.is_empty() {
                                return "排序：默认".to_string();
                            }
                            let schemas_now = schemas.get();
                            let parts: Vec<String> = sorts
                                .iter()
                                .enumerate()
                                .map(|(i, s)| {
                                    let mark = prio_mark(i);
                                    format!(
                                        "{mark} {} {}",
                                        sort_field_label(&s.field, &schemas_now),
                                        if s.desc { "↓" } else { "↑" },
                                    )
                                })
                                .collect();
                            format!("排序：{}", parts.join("  "))
                        }}</span>
                    </div>

                    {move || if show_new.get() {
                        view! {
                            <form class="filters" on:submit=create_submit>
                                <input class="inp" style="flex:1" placeholder="新条目标题" node_ref=new_title_ref
                                    prop:value=new_title on:input=move |ev| new_title.set(event_target_value(&ev)) />
                                <button class="btn pri" type="submit">"创建"</button>
                                <button class="btn" type="button" on:click=move |_| {
                                    new_labels.set(Vec::new());
                                    show_new.set(false);
                                }>"取消"</button>
                                <LabelDraft rows=new_labels members=ws_members />
                            </form>
                        }.into_any()
                    } else {
                        view! { <div></div> }.into_any()
                    }}

                    {move || error.get().map(|e| view! { <p class="error" style="padding:8px 16px">{e}</p> })}

                    <div class=move || {
                        if selected.get().is_empty() {
                            "view-body full".to_string()
                        } else {
                            "view-body".to_string()
                        }
                    }>
                        <div>
                            {move || {
                                let n = batch_selected.get().len();
                                (n > 0).then(|| view! {
                                    <div class="batchbar">
                                        <span class="mut">{format!("已选 {n} 项")}</span>
                                        <button class="btn pri sm" on:click=open_batch>"批量设置标签"</button>
                                        <button class="btn sm" disabled=move || batch_busy.get() || ai_busy.get()
                                            on:click=open_ai>"AI 总结"</button>
                                        <button class="btn sm" disabled=move || batch_busy.get()
                                            on:click=archive_selected>"归档"</button>
                                        <button class="btn sm" on:click=move |_| batch_selected.set(Vec::new())>
                                            "清除选择"
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
                                        batch_selected
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
                                    <span class="mut">{move || format!("共 {} 条", total_signal.get())}</span>
                                </div>
                            })}
                        </div>
                        <EntryPanel code=selected slug=slug().to_string() workspace_id=ws_id schemas members=ws_members refresh />
                    </div>
                </div>
            </div>

            {move || show_batch.get().then(|| view! {
                <div class="dmodal" on:click=move |_| show_batch.set(false)>
                    <div class="panel dmbox" style="max-width:560px" on:click=|ev| ev.stop_propagation()>
                        <h3>"批量设置标签"</h3>
                        <p class="mut">{move || format!(
                            "写入选中的 {} 个条目；留空的标签不会改动。", batch_selected.get().len()
                        )}</p>
                        <LabelDraft rows=batch_labels members=ws_members />
                        {move || batch_error.get().map(|e| view! { <p class="error">{e}</p> })}
                        <div style="display:flex;gap:8px;justify-content:flex-end">
                            <button class="btn" on:click=move |_| show_batch.set(false)>"取消"</button>
                            <button class="btn pri" disabled=move || batch_busy.get() on:click=apply_batch>
                                "应用"
                            </button>
                        </div>
                    </div>
                </div>
            })}

            {move || show_ai.get().then(|| view! {
                <div class="dmodal" on:click=move |_| show_ai.set(false)>
                    <div class="panel dmbox" style="max-width:520px" on:click=|ev| ev.stop_propagation()>
                        <h3>"AI 总结"</h3>
                        <p class="mut">{move || format!(
                            "把选中的 {} 个条目交给模型，生成一条新条目并全屏打开。",
                            batch_selected.get().len()
                        )}</p>
                        {move || if ai_loading.get() {
                            view! { <p class="mut">"正在读取场景与语气…"</p> }.into_any()
                        } else {
                            let scenarios = ai_scenarios.get();
                            let tones = ai_tones.get();
                            let none_configured = scenarios.is_empty() && tones.is_empty();
                            view! {
                                <div class="stack">
                                    {none_configured.then(|| view! {
                                        <p class="mut">"尚未配置场景与语气；不指定也可以直接生成，配置入口在工作空间设置页。"</p>
                                    })}
                                    <label class="fld">
                                        <span>"场景"</span>
                                        <select class="inp" prop:value=move || ai_scenario.get()
                                            on:change=move |ev| ai_scenario.set(event_target_value(&ev))>
                                            <option value="">"（不指定）"</option>
                                            {scenarios.into_iter().map(|p| {
                                                let v = p.name.clone();
                                                view! { <option value=v>{p.name}</option> }
                                            }).collect::<Vec<_>>()}
                                        </select>
                                    </label>
                                    <label class="fld">
                                        <span>"语气"</span>
                                        <select class="inp" prop:value=move || ai_tone.get()
                                            on:change=move |ev| ai_tone.set(event_target_value(&ev))>
                                            <option value="">"（不指定）"</option>
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
                            <button class="btn" on:click=move |_| show_ai.set(false)>"取消"</button>
                            <button class="btn pri" disabled=move || ai_busy.get() || ai_loading.get()
                                on:click=apply_ai>
                                {move || if ai_busy.get() { "生成中…" } else { "生成" }}
                            </button>
                        </div>
                    </div>
                </div>
            })}

            {move || archived_open.get().then(|| view! {
                <div class="dmodal" on:click=move |_| archived_open.set(false)>
                    <div class="panel dmbox" style="width:720px;max-width:92vw" on:click=|ev| ev.stop_propagation()>
                        <h3>"已归档条目"</h3>
                        <p class="mut">"归档的条目不出现在视图与全文检索里，数据与标签都保留；可随时取消归档。"</p>
                        {move || archived_error.get().map(|e| view! { <p class="error">{e}</p> })}
                        <div class="archlist">
                            {move || if archived_busy.get() {
                                view! { <p class="mut">"加载中…"</p> }.into_any()
                            } else if archived_list.get().is_empty() {
                                view! { <p class="mut">"暂无已归档条目。"</p> }.into_any()
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
                                                "取消归档"
                                            </button>
                                        </div>
                                    }
                                }).collect::<Vec<_>>().into_any()
                            }}
                        </div>
                        <div style="display:flex;justify-content:flex-end">
                            <button class="btn" on:click=move |_| archived_open.set(false)>"关闭"</button>
                        </div>
                    </div>
                </div>
            })}

            {move || if expr_help.get() {
                view! {
                    <div class="dmodal" on:click=move |_| expr_help.set(false)>
                        <div class="panel dmbox" style="max-width:560px" on:click=|ev| ev.stop_propagation()>
                            <h3>"标签表达式语法"</h3>
                            <div class="exprdoc">
                                <p>"用标签是否存在、或标签的值来筛选条目。多个条件用 "
                                    <code>"AND"</code>" / "<code>"OR"</code>" 连接，"
                                    <code>"NOT"</code>" 取反，括号可改变优先级。"</p>
                                <table class="tbl">
                                    <thead><tr><th style="width:42%">"写法"</th><th>"含义"</th></tr></thead>
                                    <tbody>
                                        <tr><td><code>"Task"</code></td><td>"打了「任务」标签"</td></tr>
                                        <tr><td><code>"!Task"</code></td><td>"没有打「任务」标签"</td></tr>
                                        <tr><td><code>"Bug AND !Task"</code></td><td>"打了 Bug 标签，没打 Task 标签"</td></tr>
                                        <tr><td><code>"Status = \"Open\""</code></td><td>"Status 等于 Open"</td></tr>
                                        <tr><td><code>"Status != \"Done\""</code></td><td>"Status 不等于 Done"</td></tr>
                                        <tr><td><code>"Status IN (\"Open\", \"Done\")"</code></td><td>"Status 是其中之一"</td></tr>
                                        <tr><td><code>"Priority >= 2"</code></td><td>"数值标签大于等于 2"</td></tr>
                                        <tr><td><code>"Summary ~ \"登录\""</code></td><td>"文本标签包含「登录」"</td></tr>
                                        <tr><td><code>"L5"</code></td><td>"直接打了 L5 标签"</td></tr>
                                        <tr><td><code>"L4+"</code></td><td>"打了 L4，或通过继承 / 覆盖关系拥有 L4"</td></tr>
                                        <tr><td><code>"!L4+"</code></td><td>"既没直接打 L4，也没有继承来 L4"</td></tr>
                                        <tr><td><code>"L4+ = \"V1\""</code></td><td>"已拥有的 L4 值等于 V1（含继承来的）"</td></tr>
                                    </tbody>
                                </table>
                                <p class="mut">"标签后面加 "<code>"+"</code>" 表示把继承 / 覆盖关系一并算进来："
                                    "在「设置 · 标签定义」里给某个标签声明关系后，它能把别的标签带进来"
                                    "（继承），或者被别的标签带出去（覆盖）。不加 "<code>"+"</code>" 时"
                                    "只匹配直接打在该条目上的标签。"</p>
                                <p class="mut">"只有 key 被继承（值没跟着传）的标签，能用于 "
                                    <code>"L4+"</code>" / "<code>"!L4+"</code>" 这类存在性判断，"
                                    "不参与等值或大小比较。"</p>
                                <p class="mut">"内置元数据："<code>"Code"</code>" / "<code>"Title"</code>" / "
                                    <code>"Detail"</code>" / "<code>"CreatedBy"</code>" / "
                                    <code>"CreatedAt"</code>" / "<code>"UpdatedBy"</code>" / "
                                    <code>"UpdatedAt"</code>"（这些名字不可用作自定义标签名）。"</p>
                                <p class="mut">"提示：在输入框里输入 "<code>"/"</code>" 可从内置元数据与本视图已有标签中选择；"
                                    "回车应用表达式，Escape 关闭提示。时间型标签（含 "<code>"CreatedAt"</code>" / "
                                    <code>"UpdatedAt"</code>"）后接比较运算符和一个空格（键与运算符之间可有空格），"
                                    "会浮出日期 / 时间选择器。"</p>
                            </div>
                            <div style="display:flex;justify-content:flex-end">
                                <button class="btn pri" on:click=move |_| expr_help.set(false)>"知道了"</button>
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
                            <h3>{move || if view_dialog_saveas.get() { "另存为新视图" } else { "新建视图" }}</h3>
                            <input class="inp" placeholder="视图名称（用 / 分层，如 团队/迭代）" prop:value=view_name_input
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
                                            <div class="mut" style="font-size:11px;padding:2px 4px">"已有非叶子节点："</div>
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
                                            "条件：全部内容".to_string()
                                        } else {
                                            format!("条件：{e}")
                                        }}
                                    </p>
                                }
                            })}
                            <div class="dfield">
                                <span class="dlabel">"展示为列的标签"</span>
                                <ColumnPicker schemas=schemas selected=view_columns_input />
                            </div>
                            <label style="display:flex;gap:6px;align-items:center">
                                <input type="checkbox" prop:checked=view_shared_input
                                    on:change=move |ev| view_shared_input.set(event_target_checked(&ev)) />
                                "共享给工作空间"
                            </label>
                            {move || dialog_error.get().map(|e| view! { <p class="error" style="margin:0">{e}</p> })}
                            <div style="display:flex;gap:8px;justify-content:flex-end">
                                <button class="btn" on:click=move |_| show_view_dialog.set(false)>"取消"</button>
                                <button class="btn pri" on:click=move |_| {
                                    let Some(ws_id) = ws_id.clone() else {
                                        dialog_error.set(Some("工作空间尚未加载完成".to_string()));
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
                                }>{move || if view_dialog_saveas.get() { "另存为" } else { "创建" }}</button>
                            </div>
                        </div>
                    </div>
                }.into_any()
            } else { view! { <div></div> }.into_any() }}

            {move || if show_config_dialog.get() {
                view! {
                    <div class="dmodal">
                        <div class="panel dmbox">
                            <h3>"视图配置"</h3>
                            {move || (!active_view.get().is_some_and(|v| v.is_default)).then(|| view! {
                                // 基础视图的名字是固定概念，不提供改名；服务端也会挡下。
                                <input class="inp" placeholder="视图名称" prop:value=config_name_input
                                    on:input=move |ev| config_name_input.set(event_target_value(&ev)) />
                            })}
                            <div class="dfield">
                                <span class="dlabel">"展示为列的标签"</span>
                                <ColumnPicker schemas=schemas selected=config_columns_input />
                            </div>
                            <div class="dfield">
                                <span class="dlabel">"时间轴视图"</span>
                                <select class="inp" prop:value=config_tl_start
                                    on:change=move |ev| config_tl_start.set(event_target_value(&ev))>
                                    <option value="">"起始时间：不启用"</option>
                                    {move || label_options(schemas.get(), true)}
                                </select>
                                <select class="inp" prop:value=config_tl_end
                                    on:change=move |ev| config_tl_end.set(event_target_value(&ev))>
                                    <option value="">"结束时间：不启用"</option>
                                    {move || label_options(schemas.get(), true)}
                                </select>
                                <select class="inp" prop:value=config_tl_person
                                    on:change=move |ev| config_tl_person.set(event_target_value(&ev))>
                                    <option value="">"相关人：不展示"</option>
                                    {move || label_options(schemas.get(), false)}
                                </select>
                                <p class="mut" style="font-size:11px;margin:2px 0 0">
                                    "起止必须是同一类时间标签（都含日期，或都是纯时刻）；留空即不启用时间轴。"
                                </p>
                            </div>
                            <label style="display:flex;gap:6px;align-items:center">
                                <input type="checkbox" prop:checked=config_shared_input
                                    on:change=move |ev| config_shared_input.set(event_target_checked(&ev)) />
                                "共享给工作空间"
                            </label>
                            <div class="tc-rules">
                                <div class="tc-head">
                                    <span>"标题颜色规则"</span>
                                    <button class="btn sm" type="button" on:click=add_rule>"＋ 规则"</button>
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
                                                        placeholder=r#"条件表达式，如：Task = "Open" AND Score >= 60"#
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
                                                        let mut items: Vec<(String, String)> = BUILTIN_FIELDS
                                                            .iter()
                                                            .map(|(k, t)| (k.to_string(), t.to_string()))
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
                                                <ColorPick small=true title="标题颜色".to_string()
                                                    value=Signal::derive(move || col.get())
                                                    ws_id=ws_id
                                                    on_pick=Callback::new(move |c: String| col.set(c)) />
                                                <button class="vc-op vc-del" type="button" title="删除"
                                                    on:click=move |_| config_rules.update(|rows| rows.retain(|x| x.id != rid))>"×"</button>
                                            </div>
                                        }
                                    }
                                />
                                <p class="mut" style="font-size:11px;margin:2px 0 0">
                                    "按顺序匹配，首个命中即上色；条件支持 AND / OR / NOT 组合标签。留空的行保存时忽略。"
                                </p>
                            </div>
                            {move || dialog_error.get().map(|e| view! { <p class="error" style="margin:0">{e}</p> })}
                            <div style="display:flex;gap:8px;justify-content:flex-end">
                                <button class="btn" on:click=move |_| show_config_dialog.set(false)>"取消"</button>
                                <button class="btn pri" disabled=move || rules_loading.get() on:click=move |_| {
                                    let Some(v) = active_view.get() else {
                                        dialog_error.set(Some("请先选择或新建一个视图".to_string()));
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
                                                    dialog_error.set(Some("缺少工作空间，无法解析条件".to_string()));
                                                    return;
                                                };
                                                match parse_view_query(ws_id, &expr).await {
                                                    Ok(a) => a,
                                                    Err(e) => {
                                                        dialog_error.set(Some(format!("标题颜色规则条件无效：{e}")));
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
                                                        dialog_error.set(Some(format!("时间轴配置未保存：{e}")));
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
                                }>"保存"</button>
                            </div>
                        </div>
                    </div>
                }.into_any()
            } else { view! { <div></div> }.into_any() }}
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
    on_new: Callback<()>,
    on_delete: Callback<String>,
    /// 「已归档」入口：打开归档条目列表弹窗。
    on_archived: Callback<()>,
) -> impl IntoView {
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
                let del_id = id.clone();
                let indent = format!("padding-left:{}px", 12 + depth * 14);
                view! {
                    <div class=move || {
                             let mut c = String::from("it");
                             if is_active() { c.push_str(" on"); }
                             c
                         }
                         style=indent
                         title=name.clone()
                         on:click=move |_| on_select.run(click_id.clone())>
                        {if shared_mark {
                            ic_share().into_any()
                        } else {
                            ic_folder().into_any()
                        }}
                        <span class="lbl" style="flex:1">{leaf_label}</span>
                        <span class="n">{count}</span>
                        <button class="ibtn" title="删除视图" on:click=move |ev| {
                            ev.stop_propagation();
                            on_delete.run(del_id.clone());
                        }>"×"</button>
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
                <button class="ibtn sb-toggle" title="收起/展开侧栏" on:click=move |_| {
                    let v = !collapsed.get_untracked();
                    collapsed.set(v);
                    set_sidebar_collapsed(v);
                }>{move || if collapsed.get() { "»" } else { "«" }}</button>
            </div>
            {move || default_view().map(|v| {
                let id = v.id.clone();
                let name = v.name.clone();
                let count = v.entry_count;
                let is_active = {
                    let id = id.clone();
                    move || active.get().as_deref() == Some(id.as_str())
                };
                let click_id = id.clone();
                view! {
                    <div class=move || {
                             let mut c = String::from("it base");
                             if is_active() { c.push_str(" on"); }
                             c
                         }
                         title=name
                         on:click=move |_| on_select.run(click_id.clone())>
                        {ic_tag()}
                        <span class="lbl" style="flex:1">{name.clone()}</span>
                        <span class="n">{count}</span>
                    </div>
                }.into_any()
            })}
            <div class="grp">"我的视图"</div>
            {move || build_tree(mine()).into_iter().map(|n| row(n, false)).collect::<Vec<_>>()}
            <div class="grp">"共享视图"</div>
            {move || build_tree(shared()).into_iter().map(|n| row(n, true)).collect::<Vec<_>>()}
            <div class="it" style="color:var(--ink3)" title="新建视图" on:click=move |_| on_new.run(())>
                {ic_add()}<span class="lbl">"新建视图"</span>
            </div>
            <div style="border-top:1px solid var(--line);margin-top:8px;padding-top:8px">
                <div class="it" title="已归档条目" on:click=move |_| on_archived.run(())>
                    {ic_folder()}<span class="lbl">"已归档"</span>
                </div>
                <A href=format!("/{slug}/settings")>
                    <div class="it" title="工作空间设置">{ic_setting()}<span class="lbl">"工作空间设置"</span></div>
                </A>
                <A href="/workspaces">
                    <div class="it" title="工作空间列表">{ic_back()}<span class="lbl">"工作空间列表"</span></div>
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
    selected: RwSignal<String>,
    /// 批量操作勾选的条目 code。与 `selected`（详情面板当前条目）互相独立。
    batch_selected: RwSignal<Vec<String>>,
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
) -> impl IntoView {
    let cols = move || columns.get();
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
        <table class="tbl">
            <thead>
                <tr>
                    <th class="pick">
                        <input type="checkbox"
                            title="全选当前列表"
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
                    {sortable_th("title".to_string(), "标题".to_string())}
                    {move || cols().iter().map(|name| {
                        let title = schemas.get().into_iter()
                            .find(|s| &s.name == name)
                            .map(|s| s.title)
                            .unwrap_or_else(|| name.clone());
                        sortable_th(name.clone(), title)
                    }).collect::<Vec<_>>()}
                    {sortable_th("createdBy".to_string(), "创建人".to_string())}
                    {sortable_th("updatedAt".to_string(), "更新时间".to_string())}
                </tr>
            </thead>
            <tbody>
                {move || match data.get() {
                    None => view! {
                        <tr><td colspan="20" class="empty">"加载中…"</td></tr>
                    }.into_any(),
                    Some(Err(e)) => view! {
                        <tr><td colspan="20" class="empty error">{e.clone()}</td></tr>
                    }.into_any(),
                    Some(Ok((_ws, items, _))) if items.is_empty() => view! {
                        <tr><td colspan="20" class="empty">"暂无条目，点击「新建 Entry」创建"</td></tr>
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
                            let code_for_click = e.code.clone();
                            let code_for_dbl = e.code.clone();
                            let code_for_check = e.code.clone();
                            let code_for_check_change = e.code.clone();
                            let code_for_badge = e.code.clone();
                            let labels = e.labels.clone();
                            // 继承 / 覆盖推导：只影响展示（虚线 + 来源说明），不改数据。
                            let derived = query_eval::derive_inherited(&sc, &e.labels);
                            let names = cols();
                            // 标题着色：整行 Entry 克隆进响应式闭包，规则变化即刻重算。
                            let entry_for_color = e.clone();
                            // 时间型标签的比较需要 schema（布局），随规则闭包一起克隆。
                            let schemas_for_color = sc.clone();
                            let title_text = e.title.clone();
                            // 创建人展示：优先服务端回填的账号，其次在工作空间成员表里找名字，
                            // 账号已删除或已退出工作空间时退回 id，好过显示成空。
                            let creator = e
                                .created_by_account
                                .as_ref()
                                .map(|a| a.name.clone())
                                .or_else(|| {
                                    members
                                        .iter()
                                        .find(|m| m.account_id == e.created_by)
                                        .map(member_label)
                                })
                                .unwrap_or_else(|| "—".to_string());
                            view! {
                                <tr
                                    class=move || if selected.get() == code_for_class { "sel".to_string() } else { String::new() }
                                    on:click=move |ev| {
                                        // 双击的第二次 click（detail==2）交给 dblclick 处理，
                                        // 避免单击开合逻辑与双击互斥冲突。
                                        if ev.detail() > 1 { return; }
                                        if selected.get_untracked() == code_for_click {
                                            selected.set(String::new());
                                        } else {
                                            selected.set(code_for_click.clone());
                                        }
                                    }
                                    on:dblclick=move |_| {
                                        on_open.run(code_for_dbl.clone());
                                    }
                                >
                                    <td class="pick">
                                        <label class="pick-cell" title={format!("当前页第 {row_no} 行")}>
                                            <input type="checkbox"
                                                prop:checked=move || batch_selected.get().contains(&code_for_check)
                                                // 勾选不应触发「打开详情」（单击）或「全屏页」（双击）。
                                                on:click=|ev| ev.stop_propagation()
                                                on:dblclick=|ev| ev.stop_propagation()
                                                on:change=move |ev| {
                                                    let c = code_for_check_change.clone();
                                                    if event_target_checked(&ev) {
                                                        batch_selected.update(|s| if !s.contains(&c) { s.push(c.clone()) });
                                                    } else {
                                                        batch_selected.update(|s| s.retain(|x| x != &c));
                                                    }
                                                }
                                            />
                                            // 行号：用 <span> 而不是 input 自身的 attribute，避免被读屏器念成
                                            // 「第 1 行 可勾选 复选框 1」这种多余组合。
                                            <span class="row-no">{row_no.to_string()}</span>
                                        </label>
                                    </td>
                                    <td style=move || match query_eval::title_color(
                                        &title_colors.get(), &entry_for_color, &entry_for_color.labels,
                                        &schemas_for_color,
                                    ) {
                                        Some(c) => format!("color:{c}"),
                                        None => String::new(),
                                    }>{title_text.clone()}{counts
                                        .get()
                                        .get(&code_for_badge)
                                        .copied()
                                        .filter(|n| *n > 0)
                                        .map(|n| view! {
                                            <span class="c-badge" title="评论条数">
                                                {ic_comment()}{n}
                                            </span>
                                        })}</td>
                                    {names.iter().map(|name| {
                                        // 优先直接打标；没有再看继承 / 覆盖推导来的（`source` 非空即为推导所得）。
                                        let direct = labels.iter()
                                            .find(|l| &l.label_name == name)
                                            .map(|l| l.value.clone());
                                        let lv = direct.map(|v| (v, Option::<String>::None)).or_else(|| {
                                            derived.iter()
                                                .find(|(n, _, _)| n == name)
                                                .map(|(_, v, src)| (v.clone(), Some(src.clone())))
                                        });
                                        match lv {
                                            None => view! { <td class="mut">"—"</td> }.into_any(),
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
                                                    .map(|src| format!("由「{src}」继承而来，未直接打在本条上"));
                                                let inh = source.is_some();
                                                match color {
                                                    Some(c) => {
                                                        let style = format!(
                                                            "border-color:{c};background:color-mix(in srgb, {c} 15%, transparent);color:{c}"
                                                        );
                                                        let cls = if inh { "chip inh" } else { "chip" };
                                                        view! {
                                                            <td><span class=cls style=style title=hint>{text}</span></td>
                                                        }.into_any()
                                                    }
                                                    None if is_enum => {
                                                        let cls = label_chip_class(name, &s);
                                                        let cls = if inh { format!("chip {cls} inh") } else { format!("chip {cls}") };
                                                        view! {
                                                            <td><span class=cls title=hint>{text}</span></td>
                                                        }.into_any()
                                                    }
                                                    None if is_null => {
                                                        let cls = if inh { "chip inh" } else { "chip" };
                                                        view! { <td><span class=cls title=hint>{text}</span></td> }.into_any()
                                                    }
                                                    None => {
                                                        view! { <td title=hint>{text}</td> }.into_any()
                                                    }
                                                }
                                            }
                                        }
                                    }).collect::<Vec<_>>()}
                                    <td class="mut">{creator}</td>
                                    <td class="mut">{short_time(&e.updated_at)}</td>
                                </tr>
                            }
                        }).collect::<Vec<_>>().into_any()
                    }
                }}
            </tbody>
        </table>
    }
}

/// 右侧详情面板的三个页签。
const DETAIL_TABS: &[(&str, &str)] =
    &[("detail", "详情"), ("attachments", "附件"), ("history", "历史")];

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
    let navigate = use_navigate();
    let data: RwSignal<Option<Result<Entry, String>>> = RwSignal::new(None);
    let labels = RwSignal::new(Vec::<Labeling>::new());
    let title = RwSignal::new(String::new());
    let detail = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
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
                    Ok(None) => data.set(Some(Err("条目不存在".to_string()))),
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
    let on_editor_change = Callback::new(move |d: String| detail.set(d));

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
                    error.set(Some(e.clone()));
                    if e.contains("内容已被他人修改") {
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
                Ok(false) => error.set(Some("删除失败".to_string())),
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
                error.set(Some("有未保存的改动，先保存再关闭".to_string()));
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
                Ok(false) => error.set(Some("归档失败".to_string())),
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
                                <button class="btn sm" on:click=open_full.clone()>{ic_full()}"全屏"</button>
                                <button class="btn pri sm" disabled=move || !dirty.get() on:click=move |_| do_save.run(())>"保存"</button>
                                <button class="btn sm" on:click=arch>"归档"</button>
                                <button class="btn danger sm" on:click=del>"删除"</button>
                                <button class="ibtn" title="关闭面板" on:click=move |_| close.run(())>{ic_close()}</button>
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
                                        title="点击编辑标题"
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
                                    <div><span class="mut">"创建人"</span>{by(&e.created_by_account)}</div>
                                    <div><span class="mut">"创建时间"</span>{fmt_datetime(&e.created_at)}</div>
                                    <div><span class="mut">"更新人"</span>{by(&e.updated_by_account)}</div>
                                    <div><span class="mut">"更新时间"</span>{fmt_datetime(&e.updated_at)}</div>
                                    {e.archived_at.clone().map(|at| view! {
                                        <div><span class="mut">"归档时间"</span>{fmt_datetime(&at)}</div>
                                    })}
                                </div>
                            }
                        })}
                        <div class="editing"><span class="dot"></span>"乐观并发 · 保存时检测冲突"</div>
                        <TabBar tabs=DETAIL_TABS active=tab />
                        {move || match tab.get().as_str() {
                            "attachments" => view! {
                                <AttachmentList
                                    code=Signal::derive(move || code.get())
                                    workspace_id=workspace_id
                                    on_changed=on_changed
                                />
                            }.into_any(),
                            "history" => view! {
                                <AuditTimeline
                                    logs=logs
                                    code=Signal::derive(move || code.get())
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
                                            view! {
                                                <TinyEditor initial entry_code=Signal::derive(move || code.get()) on_change=on_editor_change on_uploaded=on_changed />
                                            }.into_any()
                                        }
                                        _ => view! {
                                            <div class="ebody"><span class="mut">"加载中…"</span></div>
                                        }.into_any(),
                                    }}
                                </div>
                                <LabelEditor code=code schemas labels members on_changed />
                                <CommentList
                                    code=Signal::derive(move || code.get())
                                    workspace_id=workspace_id
                                    on_changed=on_changed
                                />
                            }.into_any(),
                        }}
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
                                        <span>{move || if flag.get() == Some(true) { chip.clone() } else { "打上".to_string() }}</span>
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
                                        <option value="" selected=move || sel.get().is_empty()>"（未设置）"</option>
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
                                        <option value="" selected=move || flag.get().is_none()>"（未设置）"</option>
                                        <option value="true" selected=move || flag.get() == Some(true)>"是"</option>
                                        <option value="false" selected=move || flag.get() == Some(false)>"否"</option>
                                    </select>
                                </div>
                            }.into_any()
                        } else if r.value_type == "account" {
                            let sel = r.text;
                            // AccountPicker 现在要的是 Vec<String>，这里把单串双向映射——
                            // 多选时塞逗号分隔，单选时仅取首个。
                            let picked = Callback::new(move |ids: Vec<String>| {
                                if r.multi {
                                    sel.set(ids.join(","));
                                } else {
                                    sel.set(ids.into_iter().next().unwrap_or_default());
                                }
                            });
                            let selected: Vec<String> = if r.multi {
                                sel.get().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                            } else if sel.get().is_empty() {
                                Vec::new()
                            } else {
                                vec![sel.get()]
                            };
                            view! {
                                <div class="lblrow">
                                    <span class="k">{ic_tag()}{title}</span>
                                    <AccountPicker members=members.get() selected=selected multi=r.multi
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
#[component]
fn ColumnPicker(
    schemas: RwSignal<Vec<LabelSchema>>,
    selected: RwSignal<Vec<String>>,
) -> impl IntoView {
    view! {
        <div class="colpick">
            {move || {
                let list = schemas.get();
                if list.is_empty() {
                    return view! { <span class="mut">"工作空间暂无标签"</span> }.into_any();
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

/// 排序字段的展示名：内置值有固定中文名，其余按标签名查 schema 的标题。
fn sort_field_label(field: &str, schemas: &[LabelSchema]) -> String {
    match field {
        "title" => "标题".to_string(),
        "createdAt" => "创建时间".to_string(),
        "createdBy" => "创建人".to_string(),
        "updatedBy" => "更新人".to_string(),
        "updatedAt" => "更新时间".to_string(),
        other => schemas
            .iter()
            .find(|s| s.name == other)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| other.to_string()),
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
