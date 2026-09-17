//! Go 参考时间布局串的最小实现：不依赖 chrono，后端与 wasm 前端共用。
//!
//! 按 token **最长匹配**扫描布局串，未识别的字符原样输出（格式化）/ 逐字符匹配（解析）。
//! 支持的 token：`2006` `06` `01` `1` `02` `2` `15` `03` `04` `05`
//! `Jan` `January` `Mon` `Monday` `PM` `pm`。
//!
//! 对外（标签定义里存的 `format`、配置界面展示的串）一律用**常规表示法**
//! （`YYYY-MM-DD`、`HH:mm:ss`），Go 布局只在本模块内部作为解析/格式化的实现细节。
//! 转换发生在边界：`to_go` 入、`to_pattern` 出。历史数据里可能存着 Go 布局
//! （任何含 ASCII 数字的串都按 Go 布局对待），由 `resolve` / `display_pattern`
//! 兼容读取。

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct YmdHms {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

pub const DATE_LAYOUT: &str = "2006-01-02";
pub const TIME_LAYOUT: &str = "15:04:05";
pub const DATETIME_LAYOUT: &str = "2006-01-02 15:04:05";

pub const DATE_PATTERN: &str = "YYYY-MM-DD";
pub const TIME_PATTERN: &str = "HH:mm:ss";
pub const DATETIME_PATTERN: &str = "YYYY-MM-DD HH:mm:ss";

/// 常规模式 token → Go 布局 token。顺序即最长匹配优先级，
/// 同一首字符下长 token 必须排在短 token 之前（`MMM` 在 `MM` 在 `M` 之前）。
const PATTERN_TOKENS: &[(&str, &str)] = &[
    ("YYYY", "2006"),
    ("MMMM", "January"),
    ("dddd", "Monday"),
    ("MMM", "Jan"),
    ("ddd", "Mon"),
    ("YY", "06"),
    ("MM", "01"),
    ("DD", "02"),
    ("HH", "15"),
    ("hh", "03"),
    ("mm", "04"),
    ("ss", "05"),
    ("M", "1"),
    ("D", "2"),
    ("A", "PM"),
    ("a", "pm"),
];

/// Go 布局 token → 常规模式 token，同样按最长匹配优先。
const LAYOUT_TOKENS: &[(&str, &str)] = &[
    ("January", "MMMM"),
    ("Monday", "dddd"),
    ("2006", "YYYY"),
    ("Jan", "MMM"),
    ("Mon", "ddd"),
    ("06", "YY"),
    ("01", "MM"),
    ("02", "DD"),
    ("15", "HH"),
    ("03", "hh"),
    ("04", "mm"),
    ("05", "ss"),
    ("PM", "A"),
    ("pm", "a"),
    ("1", "M"),
    ("2", "D"),
];

/// 取 `value_type` 的默认 Go 布局（后端 `default_layout` 的字符串版，供 wasm 端共用）。
pub fn default_go(value_type: &str) -> &'static str {
    match value_type {
        "date" => DATE_LAYOUT,
        "time" => TIME_LAYOUT,
        _ => DATETIME_LAYOUT,
    }
}

/// 取 `value_type` 的默认常规模式。
pub fn default_pattern(value_type: &str) -> &'static str {
    match value_type {
        "date" => DATE_PATTERN,
        "time" => TIME_PATTERN,
        _ => DATETIME_PATTERN,
    }
}

/// 时间型标签配置界面里的常用格式候选（下拉框）。
pub fn presets(value_type: &str) -> &'static [&'static str] {
    match value_type {
        "date" => &[
            "YYYY-MM-DD",
            "YYYY/MM/DD",
            "YYYYMMDD",
            "YYYY年MM月DD日",
            "MM/DD/YYYY",
        ],
        "time" => &["HH:mm:ss", "HH:mm", "HHmmss"],
        _ => &[
            "YYYY-MM-DD HH:mm:ss",
            "YYYY-MM-DD HH:mm",
            "YYYYMMDD HHmmss",
            "YYYY/MM/DD HH:mm:ss",
            "YYYY年MM月DD日 HH:mm:ss",
        ],
    }
}

/// 常规模式 → Go 布局。出现未识别的字母视为笔误返回 `None`
/// （否则 `YYYY/MM/DD at HH:mm` 里的 `at` 会被静默当成字面量）；
/// 一个 token 都没匹配到同样返回 `None`。
pub fn to_go(pattern: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = pattern;
    let mut hit = false;
    while !rest.is_empty() {
        if let Some((tok, go)) = PATTERN_TOKENS.iter().find(|(t, _)| rest.starts_with(t)) {
            out.push_str(go);
            rest = &rest[tok.len()..];
            hit = true;
        } else {
            let c = rest.chars().next().unwrap();
            if c.is_alphabetic() {
                return None;
            }
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    hit.then_some(out)
}

/// Go 布局 → 常规模式（展示历史数据用；非 token 的字符原样保留）。
pub fn to_pattern(layout: &str) -> String {
    let mut out = String::new();
    let mut rest = layout;
    while !rest.is_empty() {
        if let Some((tok, pat)) = LAYOUT_TOKENS.iter().find(|(t, _)| rest.starts_with(t)) {
            out.push_str(pat);
            rest = &rest[tok.len()..];
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// 库里的 `format` → 交给 `parse` / `format` 的 Go 布局。
/// 空 → `default_layout`；含 ASCII 数字 → 历史 Go 布局原样使用；
/// 否则按常规模式转换，转换失败退回默认（schema 写入时已校验过，正常到不了这里）。
pub fn resolve(stored: Option<&str>, default_layout: &str) -> String {
    let Some(s) = stored.map(str::trim).filter(|s| !s.is_empty()) else {
        return default_layout.to_string();
    };
    if s.bytes().any(|b| b.is_ascii_digit()) {
        return s.to_string();
    }
    to_go(s).unwrap_or_else(|| default_layout.to_string())
}

/// 库里的 `format` → 配置界面展示的常规模式；空则给该类型的默认模式。
pub fn display_pattern(stored: Option<&str>, default_pattern: &str) -> String {
    let Some(s) = stored.map(str::trim).filter(|s| !s.is_empty()) else {
        return default_pattern.to_string();
    };
    if s.bytes().any(|b| b.is_ascii_digit()) {
        return to_pattern(s);
    }
    s.to_string()
}

#[derive(Clone, Copy, PartialEq)]
enum Tok {
    Year4, Year2, Month2, Month1, Day2, Day1,
    Hour24, Hour12, Minute, Second,
    MonAbbr, MonFull, WdAbbr, WdFull, PmUpper, PmLower,
}

/// 顺序即优先级：多字符 token 必须排在它的单字符前缀之前。
const TOKENS: &[(&str, Tok)] = &[
    ("January", Tok::MonFull),
    ("Monday", Tok::WdFull),
    ("2006", Tok::Year4),
    ("Jan", Tok::MonAbbr),
    ("Mon", Tok::WdAbbr),
    ("06", Tok::Year2),
    ("01", Tok::Month2),
    ("02", Tok::Day2),
    ("15", Tok::Hour24),
    ("03", Tok::Hour12),
    ("04", Tok::Minute),
    ("05", Tok::Second),
    ("PM", Tok::PmUpper),
    ("pm", Tok::PmLower),
    ("1", Tok::Month1),
    ("2", Tok::Day1),
];

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June",
    "July", "August", "September", "October", "November", "December",
];
const WEEKDAYS: [&str; 7] = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];

fn match_token(rest: &str) -> Option<(&'static str, Tok)> {
    TOKENS.iter().find(|(t, _)| rest.starts_with(t)).copied()
}

fn pad2(n: u32) -> String {
    format!("{n:02}")
}

/// Howard Hinnant days_from_civil：公历 → 距 1970-01-01 的天数。
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let (m, d) = (m as i64, d as i64);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn weekday(t: YmdHms) -> usize {
    ((days_from_civil(t.year, t.month, t.day) + 4).rem_euclid(7)) as usize
}

pub fn format(layout: &str, t: YmdHms) -> String {
    let mut out = String::new();
    let mut rest = layout;
    while !rest.is_empty() {
        if let Some((tok, kind)) = match_token(rest) {
            out.push_str(&render(kind, t));
            rest = &rest[tok.len()..];
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

fn render(kind: Tok, t: YmdHms) -> String {
    let month = MONTHS[(t.month.clamp(1, 12) - 1) as usize];
    match kind {
        Tok::Year4 => format!("{:04}", t.year),
        Tok::Year2 => format!("{:02}", t.year.rem_euclid(100)),
        Tok::Month2 => pad2(t.month),
        Tok::Month1 => t.month.to_string(),
        Tok::Day2 => pad2(t.day),
        Tok::Day1 => t.day.to_string(),
        Tok::Hour24 => pad2(t.hour),
        Tok::Hour12 => {
            let h = t.hour % 12;
            pad2(if h == 0 { 12 } else { h })
        }
        Tok::Minute => pad2(t.minute),
        Tok::Second => pad2(t.second),
        Tok::MonAbbr => month[..3].to_string(),
        Tok::MonFull => month.to_string(),
        Tok::WdAbbr => WEEKDAYS[weekday(t)][..3].to_string(),
        Tok::WdFull => WEEKDAYS[weekday(t)].to_string(),
        Tok::PmUpper => if t.hour < 12 { "AM" } else { "PM" }.to_string(),
        Tok::PmLower => if t.hour < 12 { "am" } else { "pm" }.to_string(),
    }
}

fn take_digits(s: &str, min: usize, max: usize) -> Option<(u64, &str)> {
    let b = s.as_bytes();
    let mut end = 0;
    while end < b.len() && end < max && b[end].is_ascii_digit() {
        end += 1;
    }
    if end < min || end == 0 {
        return None;
    }
    Some((s[..end].parse().ok()?, &s[end..]))
}

/// 取月份名（全称优先，否则 3 字母缩写），大小写不敏感。
fn take_month(rest: &str) -> Option<(u32, &str)> {
    if rest.len() < 3 || !rest.is_char_boundary(3) {
        return None;
    }
    for (i, name) in MONTHS.iter().enumerate() {
        if !name[..3].eq_ignore_ascii_case(&rest[..3]) {
            continue;
        }
        let take = if rest.len() >= name.len()
            && rest.is_char_boundary(name.len())
            && name.eq_ignore_ascii_case(&rest[..name.len()])
        {
            name.len()
        } else {
            3
        };
        return Some(((i + 1) as u32, &rest[take..]));
    }
    None
}

/// 取星期名并丢弃（解析时不使用，但布局里可能出现）。
fn take_weekday(rest: &str) -> Option<&str> {
    if rest.len() < 3 || !rest.is_char_boundary(3) {
        return None;
    }
    for name in WEEKDAYS.iter() {
        if !name[..3].eq_ignore_ascii_case(&rest[..3]) {
            continue;
        }
        let take = if rest.len() >= name.len()
            && rest.is_char_boundary(name.len())
            && name.eq_ignore_ascii_case(&rest[..name.len()])
        {
            name.len()
        } else {
            3
        };
        return Some(&rest[take..]);
    }
    None
}

pub fn parse(layout: &str, s: &str) -> Option<YmdHms> {
    let mut t = YmdHms::default();
    let (mut rest, mut lay) = (s, layout);
    let (mut pm, mut has_ampm) = (false, false);
    // 只有布局里真正出现了月/日 token 时才校验对应字段，否则保留 `Default` 的 0
    //（`TIME_LAYOUT = "15:04:05"` 没有日期 token，0 是合法值）。
    let (mut has_month, mut has_day) = (false, false);
    while !lay.is_empty() {
        if let Some((tok, kind)) = match_token(lay) {
            lay = &lay[tok.len()..];
            let (n, r) = match kind {
                Tok::Year4 => take_digits(rest, 4, 4)?,
                Tok::Year2 => take_digits(rest, 1, 2)?,
                Tok::Month2 | Tok::Month1 => take_digits(rest, 1, 2)?,
                Tok::Day2 | Tok::Day1 => take_digits(rest, 1, 2)?,
                Tok::Hour24 | Tok::Hour12 => take_digits(rest, 1, 2)?,
                Tok::Minute | Tok::Second => take_digits(rest, 1, 2)?,
                Tok::MonAbbr | Tok::MonFull => {
                    let (m, r) = take_month(rest)?;
                    t.month = m;
                    has_month = true;
                    rest = r;
                    continue;
                }
                Tok::WdAbbr | Tok::WdFull => {
                    rest = take_weekday(rest)?;
                    continue;
                }
                Tok::PmUpper | Tok::PmLower => {
                    if rest.len() >= 2 && rest.is_char_boundary(2) && rest[..2].eq_ignore_ascii_case("PM")
                    {
                        pm = true;
                    } else if rest.len() >= 2
                        && rest.is_char_boundary(2)
                        && rest[..2].eq_ignore_ascii_case("AM")
                    {
                        pm = false;
                    } else {
                        return None;
                    }
                    has_ampm = true;
                    rest = &rest[2..];
                    continue;
                }
            };
            rest = r;
            match kind {
                Tok::Year4 => t.year = n as i32,
                Tok::Year2 => t.year = 2000 + n as i32,
                Tok::Month2 | Tok::Month1 => {
                    t.month = n as u32;
                    has_month = true;
                }
                Tok::Day2 | Tok::Day1 => {
                    t.day = n as u32;
                    has_day = true;
                }
                Tok::Hour24 | Tok::Hour12 => t.hour = n as u32,
                Tok::Minute => t.minute = n as u32,
                Tok::Second => t.second = n as u32,
                _ => {}
            }
        } else {
            let c = lay.chars().next().unwrap();
            let got = rest.chars().next()?;
            // 空白宽松匹配，其余逐字符严格匹配。
            if !(c == got || (c.is_whitespace() && got.is_whitespace())) {
                return None;
            }
            lay = &lay[c.len_utf8()..];
            rest = &rest[got.len_utf8()..];
        }
    }
    if !rest.trim().is_empty() {
        return None;
    }
    if has_ampm {
        if pm && t.hour < 12 {
            t.hour += 12;
        } else if !pm && t.hour == 12 {
            t.hour = 0;
        }
    }
    if (has_month && !(1..=12).contains(&t.month))
        || (has_day && !(1..=31).contains(&t.day))
        || t.hour > 23
        || t.minute > 59
        || t.second > 60
    {
        return None;
    }
    Some(t)
}
