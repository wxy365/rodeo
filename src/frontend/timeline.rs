//! 时间轴视图：把同一个视图的条目按两个时间型标签铺成横向轴上的错位色块。
//!
//! 「排布」（解析跨度、分泳道、选刻度）与「渲染」分开：前者是一组无副作用的函数，
//! 后者只把结果摆成 DOM。这样刻度会不会太密、泳道会不会重叠，可以脱离界面单独想清楚。

use leptos::prelude::*;

use crate::frontend::components::{member_label, PRESET_COLORS};
use crate::frontend::graphql_client::{
    Entry, LabelSchema, Member, ViewTimeline, Workspace, TIMELINE_MAX_ENTRIES,
};
use crate::frontend::query_eval::{self, DerivedLabel};
use crate::golayout;

/// 时间型标签的「族」。与后端 `service::view::TimeFamily` 同构——
/// 前端不能拿服务端的判据（那是 `LabelSchema` 的领域类型，客户端有自己的一份），
/// 故各写一份，改一处必须改另一处。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// 含日期：`date` / `datetime`。
    DateTime,
    /// 纯时刻：`time`。整条轴固定为一天。
    TimeOfDay,
}

/// 两个标签同族才返回 `Some`。纯时刻与含日期不能混——量纲不同。
pub fn family(start_vt: &str, end_vt: &str) -> Option<Family> {
    match (start_vt, end_vt) {
        ("date" | "datetime", "date" | "datetime") => Some(Family::DateTime),
        ("time", "time") => Some(Family::TimeOfDay),
        _ => None,
    }
}

/// 一个已排期的条目。
pub struct Placed {
    /// 在 `data` 的条目列表里的下标。
    pub idx: usize,
    /// 绝对秒区间。
    pub start: i64,
    pub end: i64,
    /// 泳道号（0 在最上面）。
    pub lane: usize,
}

/// 整套排布结果。
pub struct Plan {
    pub family: Family,
    /// 轴的秒区间。
    pub lo: i64,
    pub hi: i64,
    /// 刻度步长（秒）。
    pub step: i64,
    /// 横向比例。
    pub px_per_sec: f64,
    pub placed: Vec<Placed>,
    /// 泳道数。
    pub lanes: usize,
    /// 起止时间缺失或解析不出来的条目下标，按原顺序，画在轴下方的「未排期」区。
    pub unscheduled: Vec<usize>,
}

/// 标签值的解析布局。与表格列、筛选表达式走同一套（`resolve` 兼容历史 Go 布局）。
fn layout_of(s: &LabelSchema) -> String {
    golayout::resolve(s.format.as_deref(), golayout::default_go(&s.value_type))
}

/// 取条目的某个标签值：优先直接打标，其次继承 / 覆盖推导来的（与表格同一套口径）。
fn label_value(entry: &Entry, derived: &[DerivedLabel], name: &str) -> Option<serde_json::Value> {
    entry
        .labels
        .iter()
        .find(|l| l.label_name == name)
        .map(|l| l.value.clone())
        .or_else(|| {
            derived
                .iter()
                .find(|(n, _, _)| n.as_str() == name)
                .map(|(_, v, _)| v.clone())
        })
}

/// 条目的起止时刻（绝对秒）。任一端缺失、或按布局解析不出来，都返回 `None`。
fn span(entry: &Entry, derived: &[DerivedLabel], start: &LabelSchema, end: &LabelSchema) -> Option<(i64, i64)> {
    let parse = |s: &LabelSchema| {
        let raw = label_value(entry, derived, &s.name)?;
        let t = golayout::parse(&layout_of(s), raw.as_str()?)?;
        Some(golayout::to_seconds(t))
    };
    let a = parse(start)?;
    let b = parse(end)?;
    // 结束早于开始是录入错误：归零成「起点的瞬时块」，好过画出一个负宽度。
    Some((a, b.max(a)))
}

/// 含日期族的轴两端：覆盖全部区间；全部落在一个瞬间时撑开成一天，
/// 否则整条轴会缩成一个点，左边界等于右边界。
fn date_bounds(placed: &[Placed]) -> (i64, i64) {
    const MIN_SPAN: i64 = 86_400;
    let mut lo = placed.iter().map(|p| p.start).min().unwrap_or(0);
    let mut hi = placed.iter().map(|p| p.end).max().unwrap_or(0);
    if hi - lo < MIN_SPAN {
        let mid = lo + (hi - lo) / 2;
        lo = mid - MIN_SPAN / 2;
        hi = lo + MIN_SPAN;
    }
    (lo, hi)
}

/// 横向比例（像素 / 秒）。含日期族按「整条轴约 900px」折算，再夹在每天 8–120px：
/// 太挤看不清重叠，太疏要横向滚很久。
fn px_per_sec(span: i64, family: Family) -> f64 {
    match family {
        Family::DateTime => {
            let days = (span.max(1) as f64) / 86_400.0;
            (900.0 / days).clamp(8.0, 120.0) / 86_400.0
        }
        // 纯时刻的轴恒为一天，按「整条轴约 1200px」折算，即每小时 50px。
        Family::TimeOfDay => 50.0 / 3_600.0,
    }
}

/// 刻度步长：从固定梯子上挑第一个能让刻度数不超过 40 的。
/// 用固定秒数而不是「月 / 年」这种变长步，是因为刻度标签由 `from_seconds` 精确反算，
/// 步长不落在自然历法边界上也不影响读数——标签上的日期本身就是对的。
fn tick_step(span: i64, family: Family) -> i64 {
    const DATE_STEPS: [i64; 16] = [
        3_600, 21_600, 43_200, 86_400, 172_800, 604_800, 1_209_600, 2_592_000, 7_776_000,
        15_552_000, 31_536_000, 63_072_000, 157_680_000, 315_360_000, 788_400_000, 1_576_800_000,
    ];
    const TIME_STEPS: [i64; 9] = [300, 600, 900, 1_800, 3_600, 7_200, 10_800, 21_600, 43_200];
    const MAX_TICKS: i64 = 40;
    let steps: &[i64] = if family == Family::TimeOfDay {
        &TIME_STEPS
    } else {
        &DATE_STEPS
    };
    steps
        .iter()
        .copied()
        .find(|s| span / *s <= MAX_TICKS)
        .unwrap_or_else(|| *steps.last().unwrap())
}

/// 计算整套排布。配置里的两个标签都必须存在且同族，否则返回 `None`——
/// 调用方据此显示「配置失效」而不是画一条空轴。
pub fn plan(entries: &[Entry], schemas: &[LabelSchema], cfg: &ViewTimeline) -> Option<Plan> {
    let find = |n: &str| schemas.iter().find(|s| s.name == n);
    let (start_s, end_s) = (find(&cfg.start)?, find(&cfg.end)?);
    let family = family(&start_s.value_type, &end_s.value_type)?;

    let mut spans = Vec::new();
    let mut unscheduled = Vec::new();
    for (idx, e) in entries.iter().enumerate() {
        // 推导结果每条算一次：起止和相关人都可能是继承 / 覆盖来的。
        let derived = query_eval::derive_inherited(schemas, &e.labels);
        match span(e, &derived, start_s, end_s) {
            Some((a, b)) => spans.push((idx, a, b)),
            None => unscheduled.push(idx),
        }
    }

    // 先按时间排再分泳道：泳道号只取决于时间区间与输入下标，与哈希顺序无关，
    // 于是同一批数据每次渲染的分色都是同一套。
    spans.sort_by_key(|&(idx, a, b)| (a, b, idx));

    // 泳道分配：首个「右端点不越过本块起点」的空泳道。零长度块按占 1 秒算，
    // 否则同一时刻上的多个瞬时条目会叠成一条。
    let mut lane_ends: Vec<i64> = Vec::new();
    let mut placed = Vec::new();
    for (idx, a, b) in spans {
        let lane = match lane_ends.iter().position(|&e| a >= e) {
            Some(i) => i,
            None => {
                lane_ends.push(0);
                lane_ends.len() - 1
            }
        };
        lane_ends[lane] = b.max(a + 1);
        placed.push(Placed { idx, start: a, end: b, lane });
    }
    let lanes = lane_ends.len();

    let (lo, hi) = match family {
        // 纯时刻：轴恒为一天，横向位置直接就是「一天里的第几秒」。
        // 自动缩放会把 30 分钟的区间拉满整屏，反而不便比较。
        Family::TimeOfDay => (0, 86_400),
        Family::DateTime => date_bounds(&placed),
    };
    Some(Plan {
        family,
        lo,
        hi,
        step: tick_step(hi - lo, family),
        px_per_sec: px_per_sec(hi - lo, family),
        placed,
        lanes,
        unscheduled,
    })
}

/// 刻度所在的秒位：`lo` 之后第一个 step 的整数倍开始，到 `hi`。
pub fn ticks(lo: i64, hi: i64, step: i64) -> Vec<i64> {
    let mut out = Vec::new();
    let mut t = (lo.div_euclid(step) + 1) * step;
    while t <= hi {
        out.push(t);
        t += step;
    }
    out
}

/// 秒位 → 横向像素。
pub fn x_of(secs: i64, lo: i64, px_per_sec: f64) -> f64 {
    (secs - lo) as f64 * px_per_sec
}

/// 刻度文字。含日期族在步长小于一天时带上时刻，长步长只给日期。
fn tick_label(secs: i64, step: i64, family: Family) -> String {
    let t = golayout::from_seconds(secs);
    match family {
        Family::TimeOfDay => format!("{:02}:{:02}", t.hour, t.minute),
        Family::DateTime if step < 86_400 => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            t.year, t.month, t.day, t.hour, t.minute
        ),
        Family::DateTime => format!("{:04}-{:02}-{:02}", t.year, t.month, t.day),
    }
}

/// 色块配色：复用标签色 / 标题色那套标准色，但跳过末尾的灰——在时间轴上
/// 一块灰会被读成「无数据」。按排布顺序轮转，同一批数据每次渲染颜色一致。
fn block_color(i: usize) -> &'static str {
    PRESET_COLORS[i % (PRESET_COLORS.len() - 1)]
}

/// 相关人的显示名：账号 id → 成员姓名；成员已退出工作空间时退回 id。
fn person_name(
    entry: &Entry,
    derived: &[DerivedLabel],
    name: &str,
    members: &[Member],
) -> Option<String> {
    let id = label_value(entry, derived, name)?;
    let id = id.as_str()?;
    Some(
        members
            .iter()
            .find(|m| m.account_id == id)
            .map(member_label)
            .unwrap_or_else(|| id.to_string()),
    )
}

/// 泳道高度；`.tl-block` 高 28px + 6px 间距。与 `style/main.css` 保持一致。
const LANE_H: f64 = 34.0;

/// 时间轴渲染。外层只在「该视图配了时间轴」且开关切到时间轴时才挂载。
#[component]
pub fn TimelineView(
    /// 与 `EntryTable` 同一份数据（`data` signal 是页面上唯一的条目来源）：
    /// 时间轴模式下这里装的是**全部**命中条目，不是某一页。
    data: RwSignal<Option<Result<(Workspace, Vec<Entry>, Vec<LabelSchema>), String>>>,
    schemas: RwSignal<Vec<LabelSchema>>,
    members: RwSignal<Vec<Member>>,
    /// 当前视图的时间轴配置。
    config: Signal<Option<ViewTimeline>>,
    /// 单击色块 → 写进这里，右侧详情面板据此打开（与表格行同一套交互）。
    selected: RwSignal<String>,
    /// 双击色块 → 打开条目全屏页。
    on_open: Callback<String>,
) -> impl IntoView {
    view! {
        <div class="tl">
            {move || {
                let Some(cfg) = config.get() else {
                    return ().into_any();
                };
                let entries = match data.get() {
                    None => return view! { <p class="empty">"加载中…"</p> }.into_any(),
                    Some(Err(e)) => {
                        return view! { <p class="empty error">{e}</p> }.into_any()
                    }
                    Some(Ok((_ws, items, _))) => items,
                };
                let sc = schemas.get();
                let ms = members.get();
                let Some(p) = plan(&entries, &sc, &cfg) else {
                    return view! {
                        <p class="empty">
                            "时间轴引用的标签已不存在或类型不再匹配，请在「视图配置」里重设。"
                        </p>
                    }
                    .into_any();
                };
                let Plan { family, lo, hi, step, px_per_sec, placed, lanes, unscheduled } = p;
                let width = ((hi - lo) as f64 * px_per_sec).max(1.0);
                let lanes_h = lanes.max(1) as f64 * LANE_H;

                let tick_rows: Vec<_> = ticks(lo, hi, step)
                    .into_iter()
                    .map(|secs| {
                        let x = x_of(secs, lo, px_per_sec);
                        view! {
                            <div class="tl-tick" style=format!("left:{x:.1}px")>
                                <span>{tick_label(secs, step, family)}</span>
                            </div>
                        }
                    })
                    .collect();

                let blocks: Vec<_> = placed
                    .into_iter()
                    .map(|pl| {
                        let e = entries[pl.idx].clone();
                        let derived = query_eval::derive_inherited(&sc, &e.labels);
                        let person = cfg
                            .person
                            .as_deref()
                            .and_then(|n| person_name(&e, &derived, n, &ms));
                        let left = x_of(pl.start, lo, px_per_sec);
                        // 零长度的块也要看得见，给 4px 兜底宽度。
                        let w = (x_of(pl.end, lo, px_per_sec) - left).max(4.0);
                        let top = pl.lane as f64 * LANE_H;
                        let color = block_color(pl.idx);
                        let title = e.title.clone();
                        (e.code.clone(), left, top, w, color, title, person)
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|(code, left, top, w, color, title, person)| {
                        let c_sel = code.clone();
                        let c_click = code.clone();
                        let c_dbl = code;
                        let t_tip = title.clone();
                        view! {
                            <div
                                class=move || {
                                    if selected.get() == c_sel { "tl-block sel" } else { "tl-block" }
                                }
                                style=format!(
                                    "left:{left:.1}px;top:{top:.1}px;width:{w:.1}px;\
                                     border-color:{color};\
                                     background:color-mix(in srgb, {color} 15%, transparent);\
                                     color:{color}",
                                )
                                title=t_tip
                                on:click=move |ev: leptos::ev::MouseEvent| {
                                    // 双击的第二次 click（detail==2）交给 dblclick，
                                    // 否则单击的选中/取消会在跳转前先闪一下。
                                    if ev.detail() > 1 {
                                        return;
                                    }
                                    if selected.get_untracked() == c_click {
                                        selected.set(String::new());
                                    } else {
                                        selected.set(c_click.clone());
                                    }
                                }
                                on:dblclick=move |_| on_open.run(c_dbl.clone())
                            >
                                <span class="tl-title">{title}</span>
                                {person.map(|p| view! { <span class="tl-person">{p}</span> })}
                            </div>
                        }
                    })
                    .collect();

                let uns: Vec<_> = unscheduled
                    .into_iter()
                    .map(|idx| {
                        let e = entries[idx].clone();
                        let c_sel = e.code.clone();
                        let c_click = e.code.clone();
                        let c_dbl = e.code;
                        let title = e.title.clone();
                        view! {
                            <div
                                class=move || {
                                    if selected.get() == c_sel { "tl-uns sel" } else { "tl-uns" }
                                }
                                on:click=move |ev: leptos::ev::MouseEvent| {
                                    if ev.detail() > 1 {
                                        return;
                                    }
                                    if selected.get_untracked() == c_click {
                                        selected.set(String::new());
                                    } else {
                                        selected.set(c_click.clone());
                                    }
                                }
                                on:dblclick=move |_| on_open.run(c_dbl.clone())
                            >
                                {title}
                            </div>
                        }
                    })
                    .collect::<Vec<_>>();

                view! {
                    <div class="tl-scroll">
                        <div class="tl-canvas" style=format!("width:{width:.1}px")>
                            <div class="tl-axis">{tick_rows}</div>
                            <div class="tl-lanes" style=format!("height:{lanes_h:.1}px")>{blocks}</div>
                        </div>
                    </div>
                    {(entries.len() >= TIMELINE_MAX_ENTRIES).then(|| view! {
                        <p class="mut tl-note">
                            {format!("条目较多，仅展示前 {TIMELINE_MAX_ENTRIES} 条")}
                        </p>
                    })}
                    <div class="tl-uns-wrap">
                        <span class="tl-uns-head">
                            {format!("未排期（{}）", uns.len())}
                        </span>
                        {uns}
                    </div>
                }
                .into_any()
            }}
        </div>
    }
}
