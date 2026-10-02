//! Open-redirect 守卫：只允许同源相对路径。

/// `true` 当且仅当 `s` 是以 `/` 开头但不以 `//` 或 `/\\` 开头的相对路径，
/// 且不含 `://`、`\\`、CR、LF、NUL。
///
/// 显式拒绝：
/// - `https://evil.com`（外链）
/// - `//evil.com`（协议相对，浏览器按当前协议补全）
/// - `/\evil.com`（Windows 路径语法在某些浏览器下被解析成外链）
/// - `/login\nfoo`（header injection）
pub fn is_safe_return_to(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if !s.starts_with('/') {
        return false;
    }
    if s.starts_with("//") || s.starts_with("/\\") {
        return false;
    }
    // 含协议分隔符也拒 —— 比如 `/redirect?next=https://evil.com` 不算合法 return_to
    // （前端如果想拼复杂 return_to，请走 query 而不是把它整段塞 path）
    if s.contains("://") || s.contains('\\') {
        return false;
    }
    if s.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::is_safe_return_to;

    #[test]
    fn accepts_simple_paths() {
        assert!(is_safe_return_to("/"));
        assert!(is_safe_return_to("/workspaces"));
        assert!(is_safe_return_to("/workspaces/abc?x=1"));
        assert!(is_safe_return_to("/login?bind=foo&provider=wechat"));
    }

    #[test]
    fn rejects_external_urls() {
        assert!(!is_safe_return_to("https://evil.com"));
        assert!(!is_safe_return_to("http://evil.com/path"));
        assert!(!is_safe_return_to("//evil.com"));
        assert!(!is_safe_return_to("/\\evil.com"));
        assert!(!is_safe_return_to("javascript:alert(1)"));
        assert!(!is_safe_return_to("evil.com"));
    }

    #[test]
    fn rejects_protocol_inside_path() {
        assert!(!is_safe_return_to("/path/https://evil.com"));
    }

    #[test]
    fn rejects_header_injection() {
        assert!(!is_safe_return_to("/path\nfoo"));
        assert!(!is_safe_return_to("/path\rfoo"));
        assert!(!is_safe_return_to("/path\0foo"));
    }
}

