use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};

use crate::frontend::components::{
    display_enum_value, fmt_datetime, from_native, is_native_time_layout, label_chip_class,
    logged_out, short_time, value_to_string,
};
use crate::frontend::graphql_client::{
    archive_entry, archived_entries, create_entry, create_view, delete_entry, delete_view, entry,
    format_view_query, get_sidebar_collapsed, label_schemas, parse_view_query, query_entries,
    set_labeling, set_labelings, set_sidebar_collapsed, unarchive_entry, update_entry, update_view,
    views, workspace_by_slug, AccountBrief, Entry, Labeling, LabelSchema, View, Workspace,
};
use crate::frontend::icons::{
    ic_add, ic_back, ic_check, ic_close, ic_copy, ic_folder, ic_full, ic_help, ic_search,
    ic_setting, ic_share, ic_tag,
};
use crate::frontend::label_editor::LabelEditor;
use crate::frontend::query_eval;
use crate::frontend::tiny_editor::TinyEditor;
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
    fn from_schema(s: LabelSchema) -> Self {
        Self {
            name: s.name,
            title: s.title,
            value_type: match s.value_type.as_str() {
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
                _ => "string",
            },
            // 先读 `multi` / `format`，再把 `enum_values` 移进信号。
            multi: s.multi,
            format: s.format,
            enum_values: RwSignal::new(s.enum_values),
            text: RwSignal::new(String::new()),
            many: RwSignal::new(Vec::new()),
            flag: RwSignal::new(None),
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

#[component]
pub fn WorkspaceMain() -> impl IntoView {
    let params = use_params_map();
    let slug = move || params.get().get("slug").unwrap_or_default();
    let navigate = use_navigate();
    let refresh = RwSignal::new(0u32);
    let data: RwSignal<Option<Result<(Workspace, Vec<Entry>, Vec<LabelSchema>), String>>> =
        RwSignal::new(None);
    let schemas = RwSignal::new(Vec::<LabelSchema>::new());
    let ws_name = RwSignal::new(String::new());
    let selected = RwSignal::new(String::new());
    let show_new = RwSignal::new(false);
    let new_title = RwSignal::new(String::new());
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
    // 全屏详情浮层开关（浮层内复用 EntryPanel）。
    let fullscreen = RwSignal::new(false);

    // 全局 Esc：优先退出全屏浮层，否则关闭右侧详情面板。
    if cfg!(target_arch = "wasm32") {
        let handle = window_event_listener(leptos::ev::keydown, move |ev| {
            if ev.key() == "Escape" {
                if fullscreen.get_untracked() {
                    fullscreen.set(false);
                } else if !selected.get_untracked().is_empty() {
                    selected.set(String::new());
                }
            }
        });
        on_cleanup(move || handle.remove());
    }

    // 详情被关闭（selected 清空）时一并退出全屏，避免下次单击直接进入全屏浮层。
    Effect::new_sync(move |_| {
        if selected.get().is_empty() && fullscreen.get_untracked() {
            fullscreen.set(false);
        }
    });

    // ---- 筛选查询状态 ----
    let query_ast = RwSignal::new(serde_json::json!({ "and": [] }));
    // 已落库的视图基线 (query, sort_field, sort_desc)。与当前编辑态比较，无差异时
    // 「保存视图」置灰：既挡掉无效提交，也让保存成功有可见反馈（按钮重新变灰）。
    let saved_baseline: RwSignal<(Value, String, bool)> =
        RwSignal::new((serde_json::json!({ "and": [] }), "updatedAt".to_string(), true));
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
                    .map(|v| (v.query.clone(), v.sort.field.clone(), v.sort.desc))
                    .unwrap_or_else(|| {
                        (serde_json::json!({ "and": [] }), "updatedAt".to_string(), true)
                    }),
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

    // 编辑态与已落库基线的差异：查询条件或排序任一变化即视为有未保存改动。
    let view_dirty = move || {
        let Some(v) = active_view.get() else {
            return false;
        };
        let (q, f, d) = saved_baseline.get();
        query_ast.get() != q || v.sort.field != f || v.sort.desc != d
    };

    // ---- 新建视图弹窗 ----
    let show_view_dialog = RwSignal::new(false);
    let view_name_input = RwSignal::new(String::new());
    let view_shared_input = RwSignal::new(false);
    let view_columns_input = RwSignal::new(Vec::<String>::new()); // 选中展示为列的标签 name

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
        if logged_out() {
            navigate("/login", Default::default());
            return;
        }
        // 查询须在 spawn 前同步组装，signal 读取才会登记为 Effect 依赖。
        // 条件来自 query_ast（筛选芯片 / 表达式编辑它），再并入顶栏 ad-hoc 全文词。
        // R16：ad-hoc 词仅在回车时写入 query_ast，故这里用 get_untracked 读取，
        // 避免每次击键都触发重查；query_ast 才是真正的重查触发器。
        let ast = with_text(&query_ast.get(), &ad_hoc_text.get_untracked());
        let sort_field = active_view
            .get()
            .map(|v| v.sort.field)
            .unwrap_or_else(|| "updatedAt".to_string());
        let sort_desc = active_view.get().map(|v| v.sort.desc).unwrap_or(true);
        let page_now = page_signal.get();
        let my_seq = req_seq.get_untracked() + 1;
        req_seq.set(my_seq);
        spawn_local(async move {
            let fetched = async {
                let ws = workspace_by_slug(&s).await?.ok_or("工作空间不存在".to_string())?;
                let ep = query_entries(&ws.id, &ast, &sort_field, sort_desc, page_now, page_size)
                    .await?;
                let schema_list = label_schemas(&ws.id).await?;
                Ok::<_, String>((ws, ep.items, schema_list, ep.total, ep.label_names))
            }
            .await;
            // 只接受最新一次请求的结果，丢弃乱序返回的旧响应（翻页/排序并发时可能发生）。
            if req_seq.get_untracked() != my_seq {
                return;
            }
            match fetched {
                Ok((w, items, list, total, names)) => {
                    ws_name.set(w.name.clone());
                    schemas.set(list.clone());
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
                    None => format!("/{} · 默认视图「全部内容」", slug()),
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
                        view_name_input.set(String::new());
                        view_columns_input.set(Vec::new());
                        view_shared_input.set(false);
                        dialog_error.set(None);
                        show_view_dialog.set(true);
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
                        <button class="btn" on:click=move |_| {
                            let Some(v) = active_view.get() else {
                                error.set(Some("请先选择或新建一个视图".to_string()));
                                return;
                            };
                            config_name_input.set(v.name.clone());
                            config_columns_input.set(v.columns.clone());
                            config_shared_input.set(v.is_shared);
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
                                new_labels.set(
                                    schemas.get_untracked().into_iter().map(DraftLabel::from_schema).collect(),
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
                        <button class="btn" style="margin-left:auto" disabled=move || !view_dirty()
                            on:click=move |_| {
                                let Some(v) = active_view.get() else { return };
                                let ast = query_ast.get();
                                let cols = v.columns.clone();
                                let shared = v.is_shared;
                                let id = v.id.clone();
                                let name = v.name.clone();
                                let field = v.sort.field.clone();
                                let desc = v.sort.desc;
                                let title_colors = v.title_colors.clone();
                                spawn_local(async move {
                                    match update_view(&id, &name, &ast, &field, desc, &cols, shared, &title_colors).await {
                                        Ok(saved) => {
                                            view_list.update(|l| {
                                                if let Some(slot) = l.iter_mut().find(|x| x.id == saved.id) {
                                                    *slot = saved.clone();
                                                }
                                            });
                                            active_view.set(Some(saved));
                                            saved_baseline.set((ast, field, desc));
                                            error.set(None);
                                        }
                                        Err(e) => error.set(Some(e)),
                                    }
                                });
                            }>"保存视图"</button>
                        <span class="mut">{move || {
                            let (field, desc) = active_view
                                .get()
                                .map(|v| (v.sort.field, v.sort.desc))
                                .unwrap_or_else(|| ("updatedAt".to_string(), true));
                            format!("排序：{}", sort_label(&field, desc))
                        }}</span>
                    </div>

                    {move || if show_new.get() {
                        view! {
                            <form class="filters" on:submit=create_submit>
                                <input class="inp" style="flex:1" placeholder="新条目标题" prop:value=new_title on:input=move |ev| new_title.set(event_target_value(&ev)) />
                                <button class="btn pri" type="submit">"创建"</button>
                                <button class="btn" type="button" on:click=move |_| {
                                    new_labels.set(Vec::new());
                                    show_new.set(false);
                                }>"取消"</button>
                                <LabelDraft rows=new_labels />
                            </form>
                        }.into_any()
                    } else {
                        view! { <div></div> }.into_any()
                    }}

                    {move || error.get().map(|e| view! { <p class="error" style="padding:8px 16px">{e}</p> })}

                    <div class=move || {
                        if selected.get().is_empty() || fullscreen.get() {
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
                                        <button class="btn sm" disabled=move || batch_busy.get()
                                            on:click=archive_selected>"归档"</button>
                                        <button class="btn sm" on:click=move |_| batch_selected.set(Vec::new())>
                                            "清除选择"
                                        </button>
                                    </div>
                                })
                            }}
                            <EntryTable
                                data
                                schemas
                                selected
                                fullscreen
                                batch_selected
                                columns=Signal::derive(move || {
                                    active_view.get().map(|v| v.columns).unwrap_or_default()
                                })
                                sort_field=Signal::derive(move || {
                                    active_view
                                        .get()
                                        .map(|v| v.sort.field)
                                        .unwrap_or_else(|| "updatedAt".to_string())
                                })
                                sort_desc=Signal::derive(move || {
                                    active_view.get().map(|v| v.sort.desc).unwrap_or(true)
                                })
                                title_colors=Signal::derive(move || {
                                    active_view
                                        .get()
                                        .map(|v| v.title_colors)
                                        .unwrap_or(Value::Null)
                                })
                                on_sort=Callback::new(move |field: String| {
                                    // 后端 SortField 仅支持 updatedAt/createdAt/title，
                                    // 未知字段会被拒绝；这里只接受受支持字段。
                                    if !matches!(field.as_str(), "updatedAt" | "createdAt" | "title") {
                                        return;
                                    }
                                    let (cur_field, cur_desc) = active_view
                                        .get()
                                        .map(|v| (v.sort.field, v.sort.desc))
                                        .unwrap_or_else(|| ("updatedAt".to_string(), true));
                                    let desc = if field == cur_field { !cur_desc } else { true };
                                    // 同一批内改写 active_view 与 page_signal，Effect 只跑一次，
                                    // 避免并发两次请求、旧页码的结果乱序覆盖新结果。
                                    batch(move || {
                                        if let Some(mut v) = active_view.get() {
                                            v.sort.field = field;
                                            v.sort.desc = desc;
                                            active_view.set(Some(v));
                                        }
                                        page_signal.set(1);
                                    });
                                })
                            />
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
                        </div>
                        {move || if fullscreen.get() {
                            view! { <div></div> }.into_any()
                        } else {
                            view! {
                                <EntryPanel code=selected slug=slug().to_string() schemas refresh />
                            }.into_any()
                        }}
                    </div>
                </div>
            </div>

            {move || if fullscreen.get() && !selected.get().is_empty() {
                view! {
                    <div class="fullscreen">
                        <div class="fs-head">
                            <b>"全屏详情"</b>
                            <span class="mut">{move || selected.get()}</span>
                            <button class="ibtn" title="关闭 (Esc)" on:click=move |_| fullscreen.set(false)>
                                {ic_close()}
                            </button>
                        </div>
                        <div class="fs-body">
                            <EntryPanel code=selected slug=slug().to_string() schemas refresh />
                        </div>
                    </div>
                }.into_any()
            } else {
                view! { <div></div> }.into_any()
            }}

            {move || show_batch.get().then(|| view! {
                <div class="dmodal" on:click=move |_| show_batch.set(false)>
                    <div class="panel dmbox" style="max-width:560px" on:click=|ev| ev.stop_propagation()>
                        <h3>"批量设置标签"</h3>
                        <p class="mut">{move || format!(
                            "写入选中的 {} 个条目；留空的标签不会改动。", batch_selected.get().len()
                        )}</p>
                        <LabelDraft rows=batch_labels />
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
                                    </tbody>
                                </table>
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
                            <h3>"新建视图"</h3>
                            <input class="inp" placeholder="视图名称" prop:value=view_name_input
                                on:input=move |ev| view_name_input.set(event_target_value(&ev)) />
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
                                    dialog_error.set(None);
                                    spawn_local(async move {
                                        match create_view(&ws_id, &name, &serde_json::json!({"and": []}),
                                            "updatedAt", true, &cols, shared, &serde_json::json!([])).await {
                                            Ok(v) => {
                                                view_list.update(|l| l.push(v.clone()));
                                                set_active(Some(v));
                                                show_view_dialog.set(false);
                                            }
                                            Err(e) => dialog_error.set(Some(e)),
                                        }
                                    });
                                }>"创建"</button>
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
                            <input class="inp" placeholder="视图名称" prop:value=config_name_input
                                on:input=move |ev| config_name_input.set(event_target_value(&ev)) />
                            <div class="dfield">
                                <span class="dlabel">"展示为列的标签"</span>
                                <ColumnPicker schemas=schemas selected=config_columns_input />
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
                                        view! {
                                            <div class="tc-row">
                                                <input class="inp tc-expr" type="text"
                                                    placeholder=r#"条件表达式，如：Task = "Open" AND Score >= 60"#
                                                    prop:value=move || exc.get()
                                                    on:input=move |ev| exc.set(event_target_value(&ev)) />
                                                <input type="color" class="sw sm" title="标题颜色"
                                                    prop:value=move || {
                                                        let c = col.get();
                                                        if c.is_empty() { "#3b82f6".to_string() } else { c }
                                                    }
                                                    on:input=move |ev| col.set(event_target_value(&ev)) />
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
                                    let field = v.sort.field.clone();
                                    let desc = v.sort.desc;
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
                                        match update_view(&id, &name, &ast, &field, desc, &cols, shared, &title_colors).await {
                                            Ok(saved) => {
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
    // 默认视图单独置顶展示，不混进「我的 / 共享」两组。
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

    let row = move |v: View, shared_mark: bool, is_default: bool| {
        let id = v.id.clone();
        let name = v.name.clone();
        let count = v.entry_count;
        let is_active = {
            let id = id.clone();
            move || active.get().as_deref() == Some(id.as_str())
        };
        let click_id = id.clone();
        let del_id = id.clone();
        view! {
            <div class=move || if is_active() { "it on" } else { "it" }
                 on:click=move |_| on_select.run(click_id.clone())>
                {if is_default {
                    ic_tag().into_any()
                } else if shared_mark {
                    ic_share().into_any()
                } else {
                    ic_folder().into_any()
                }}
                <span class="lbl" style="flex:1">{name}</span>
                {is_default.then(|| view! { <span class="chip dim" style="font-size:11px">"默认"</span> })}
                <span class="n">{count}</span>
                {(!is_default).then(|| view! {
                    <button class="ibtn" title="删除视图" on:click=move |ev| {
                        ev.stop_propagation();
                        on_delete.run(del_id.clone());
                    }>"×"</button>
                })}
            </div>
        }
        .into_any()
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
            <div class="grp">"默认视图"</div>
            {move || default_view().map(|v| row(v, true, true))}
            <div class="grp">"我的视图"</div>
            {move || mine().into_iter().map(|v| row(v, false, false)).collect::<Vec<_>>()}
            <div class="grp">"共享视图"</div>
            {move || shared().into_iter().map(|v| row(v, true, false)).collect::<Vec<_>>()}
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

/// 动态列条目表：列来自当前视图（`columns` 里的标签 name），标题/更新时间可点击排序。
#[component]
fn EntryTable(
    data: RwSignal<Option<Result<(Workspace, Vec<Entry>, Vec<LabelSchema>), String>>>,
    schemas: RwSignal<Vec<LabelSchema>>,
    selected: RwSignal<String>,
    /// 批量操作勾选的条目 code。与 `selected`（详情面板当前条目）互相独立。
    batch_selected: RwSignal<Vec<String>>,
    /// 双击行时置 true，打开全屏详情浮层。
    fullscreen: RwSignal<bool>,
    columns: Signal<Vec<String>>,
    sort_field: Signal<String>,
    sort_desc: Signal<bool>,
    /// 当前视图的标题颜色规则 `[{query,color}]`；命中即给标题上色。
    title_colors: Signal<Value>,
    on_sort: Callback<String>,
) -> impl IntoView {
    let cols = move || columns.get();
    // 当前排序列的升降序箭头；未排序列为空。
    let arrow = move |field: &str| -> &'static str {
        if sort_field.get() == field {
            if sort_desc.get() {
                " ↓"
            } else {
                " ↑"
            }
        } else {
            ""
        }
    };
    // 可排序表头。后端 SortField 仅支持 title/updatedAt/createdAt，
    // 标签列因此不接排序，避免提交未知字段被服务端拒绝。
    let sortable_th = move |field: &'static str, label: &'static str| {
        view! {
            <th class="sortable" on:click=move |_| on_sort.run(field.to_string())>
                {move || format!("{label}{}", arrow(field))}
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
                    <th>"Code"</th>
                    {sortable_th("title", "标题")}
                    {move || cols().iter().map(|name| {
                        let title = schemas.get().into_iter()
                            .find(|s| &s.name == name)
                            .map(|s| s.title)
                            .unwrap_or_else(|| name.clone());
                        view! { <th>{title}</th> }
                    }).collect::<Vec<_>>()}
                    {sortable_th("updatedAt", "更新时间")}
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
                        items.iter().map(|e| {
                            let code = e.code.clone();
                            let code_for_class = e.code.clone();
                            let code_for_click = e.code.clone();
                            let code_for_dbl = e.code.clone();
                            let code_for_copy = e.code.clone();
                            let code_for_check = e.code.clone();
                            let code_for_check_change = e.code.clone();
                            // 复制成功后的短暂打勾反馈，逐行独立。
                            let copied = RwSignal::new(false);
                            let labels = e.labels.clone();
                            let names = cols();
                            // 标题着色：整行 Entry 克隆进响应式闭包，规则变化即刻重算。
                            let entry_for_color = e.clone();
                            // 时间型标签的比较需要 schema（布局），随规则闭包一起克隆。
                            let schemas_for_color = sc.clone();
                            let title_text = e.title.clone();
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
                                        selected.set(code_for_dbl.clone());
                                        fullscreen.set(true);
                                    }
                                >
                                    <td class="pick">
                                        <input type="checkbox"
                                            prop:checked=move || batch_selected.get().contains(&code_for_check)
                                            // 勾选不应触发「打开详情」（单击）或「全屏详情」（双击）。
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
                                    </td>
                                    <td class="code">
                                        <span class="codecell">
                                            <span>{code.clone()}</span>
                                            <button class="ibtn codecopy" title="复制编码" on:click=move |ev| {
                                                ev.stop_propagation();
                                                copy_to_clipboard(&code_for_copy);
                                                copied.set(true);
                                                set_timeout(
                                                    move || copied.set(false),
                                                    std::time::Duration::from_millis(1200),
                                                );
                                            }>
                                                {move || if copied.get() { ic_check().into_any() } else { ic_copy().into_any() }}
                                            </button>
                                        </span>
                                    </td>
                                    <td style=move || match query_eval::title_color(
                                        &title_colors.get(), &entry_for_color, &entry_for_color.labels,
                                        &schemas_for_color,
                                    ) {
                                        Some(c) => format!("color:{c}"),
                                        None => String::new(),
                                    }>{title_text.clone()}</td>
                                    {names.iter().map(|name| {
                                        let lv = labels.iter()
                                            .find(|l| &l.label_name == name)
                                            .map(|l| l.value.clone());
                                        match lv {
                                            None => view! { <td class="mut">"—"</td> }.into_any(),
                                            Some(v) => {
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
                                                match color {
                                                    Some(c) => {
                                                        let style = format!(
                                                            "background:color-mix(in srgb, {c} 15%, transparent);color:{c}"
                                                        );
                                                        view! {
                                                            <td><span class="chip" style=style>{text}</span></td>
                                                        }.into_any()
                                                    }
                                                    None if is_enum => {
                                                        let cls = label_chip_class(name, &s);
                                                        view! {
                                                            <td><span class=format!("chip {cls}")>{text}</span></td>
                                                        }.into_any()
                                                    }
                                                    None if is_null => {
                                                        view! { <td><span class="chip">{text}</span></td> }.into_any()
                                                    }
                                                    None => view! { <td>{text}</td> }.into_any(),
                                                }
                                            }
                                        }
                                    }).collect::<Vec<_>>()}
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

/// 右侧详情面板：只编辑详情与标签（标题在 Entry 全屏编辑），保存走乐观并发。
#[component]
fn EntryPanel(
    code: RwSignal<String>,
    slug: String,
    schemas: RwSignal<Vec<LabelSchema>>,
    refresh: RwSignal<u32>,
) -> impl IntoView {
    let navigate = use_navigate();
    let data: RwSignal<Option<Result<Entry, String>>> = RwSignal::new(None);
    let labels = RwSignal::new(Vec::<Labeling>::new());
    let detail = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);

    // overwrite=true：清空编辑缓冲后全量填充（选中变化 / 并发冲突重载）。
    // overwrite=false：软重载，仅更新 data/labels，保留未保存的详情编辑。
    let load = move |overwrite: bool| {
        let c = code.get();
        if c.is_empty() {
            return;
        }
        if overwrite {
            detail.set(String::new());
            labels.set(Vec::new());
            data.set(None);
            error.set(None);
        }
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let result = entry(&c).await;
                if code.get() != c {
                    return;
                }
                match result {
                    Ok(Some(e)) => {
                        labels.set(e.labels.clone());
                        if overwrite {
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

    let save = move |_| {
        let c = code.get();
        let Some(entry_now) = data.get().and_then(|r| r.ok()) else {
            return;
        };
        let expected = entry_now.updated_at.clone();
        let t = entry_now.title.clone();
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
    };

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

    let close = move |_| code.set(String::new());

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
                            <span class="code">{move || code.get()}</span>
                            <button class="ibtn" title="复制编码" on:click=move |_| copy_to_clipboard(&code.get_untracked())>{ic_copy()}</button>
                            <h3>{move || data.get().and_then(|r| r.ok()).map(|e| e.title.clone()).unwrap_or_default()}</h3>
                            <div class="dacts">
                                <button class="btn sm" on:click=open_full.clone()>{ic_full()}"全屏"</button>
                                <button class="btn pri sm" on:click=save>"保存"</button>
                                <button class="btn sm" on:click=arch>"归档"</button>
                                <button class="btn danger sm" on:click=del>"删除"</button>
                                <button class="ibtn" title="关闭面板" on:click=close>{ic_close()}</button>
                            </div>
                        </div>
                        {move || data.get().and_then(|r| r.ok()).map(|e| {
                            let by = |a: &Option<AccountBrief>| a.as_ref().map(|x| x.name.clone()).unwrap_or_else(|| "—".to_string());
                            view! {
                                <div class="dmeta">
                                    <div><span class="mut">"编码"</span><span class="code">{e.code.clone()}</span></div>
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
                        <div class="dtabs">
                            <button class="on">"详情"</button>
                            <button disabled>"附件"</button>
                            <button disabled>"历史"</button>
                        </div>
                        <div class="editor">
                            {move || match data.get() {
                                Some(Ok(e)) => {
                                    let initial = e.detail.clone();
                                    view! {
                                        <TinyEditor initial on_change=on_editor_change />
                                    }.into_any()
                                }
                                _ => view! {
                                    <div class="ebody"><span class="mut">"加载中…"</span></div>
                                }.into_any(),
                            }}
                        </div>
                        <LabelEditor code=code schemas labels on_changed />
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
fn LabelDraft(rows: RwSignal<Vec<DraftLabel>>) -> impl IntoView {
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

fn sort_label(field: &str, desc: bool) -> String {
    let name = match field {
        "title" => "标题",
        "createdAt" => "创建时间",
        "updatedAt" | "" => "更新时间",
        other => other,
    };
    format!("{name} {}", if desc { "↓" } else { "↑" })
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

/// 复制文本到系统剪贴板。非 wasm 目标下为空实现，便于 `cargo check` 通过。
#[cfg(target_arch = "wasm32")]
fn copy_to_clipboard(text: &str) {
    let Some(win) = leptos::web_sys::window() else {
        return;
    };
    // 丢弃 Promise 不影响写入：它是已排入队列的异步任务。
    let _ = win.navigator().clipboard().write_text(text);
}

#[cfg(not(target_arch = "wasm32"))]
fn copy_to_clipboard(_text: &str) {}
