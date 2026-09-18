use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Extension, Path};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use ulid::Ulid;

use crate::api::AppState;

/// 可以内联展示的类型白名单。**只放行栅格图**：SVG 能携带脚本，`text/html`
/// 更是同源存储型 XSS，一律降级成附件下载。
fn inline_content_type(reported: &str) -> Option<&'static str> {
    let base = reported
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        "image/bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// 附件下载。**免鉴权**：`<img src>` 是浏览器发起的裸请求，带不了 `Authorization`
/// 头，而本仓库从不签发 cookie。id 是不可猜的 ULID，语义等同「能力 URL」。
pub async fn download_attachment(
    Extension(state): Extension<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = Ulid::from_string(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let attachment = match state.services.attachment.get(id) {
        Ok(Some(a)) => a,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("读取附件元数据失败 {id}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let bytes = match state.services.attachment.read(&attachment).await {
        Ok(b) => b,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let inline = inline_content_type(&attachment.content_type);
    let mut resp = Response::new(Body::from(bytes));
    let headers = resp.headers_mut();
    // 绝不回声客户端上报的 contentType：上报值来自上传方，
    // 回声它等于允许上传 text/html 后在应用同源下执行脚本。
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(inline.unwrap_or("application/octet-stream")),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition(
            inline.is_some(),
            &attachment.filename,
        ))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // id 决定内容不变，可长期缓存。
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    resp
}

/// 内联图不需要文件名；其余一律 `attachment` + RFC 5987 的 `filename*`
/// （原始名可能含中文，`filename=` 的 latin-1 会乱码）。
fn content_disposition(inline: bool, filename: &str) -> String {
    if inline {
        return "inline".to_string();
    }
    format!(
        "attachment; filename*=UTF-8''{}",
        percent_encode(filename)
    )
}

/// 最小百分号编码：只保留 RFC 3986 的 unreserved 集合，其余逐字节转义。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
