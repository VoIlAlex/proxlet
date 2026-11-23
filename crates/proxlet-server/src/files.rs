//! Static file serving.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::SeekFrom;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_util::TryStreamExt;
use http::header::{self, HeaderMap, HeaderValue};
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame, Incoming};
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use proxlet_core::Settings;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::Body;
use crate::response::{self, SERVER};
use crate::snapshot::FilesTarget;

/// Characters escaped in links inside a directory listing.
const LINK: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

/// Serves a request matched by a files rule. `prefix` is the rule's path.
pub(crate) async fn serve(
    settings: &Settings,
    target: &FilesTarget,
    prefix: &str,
    req: &Request<Incoming>,
) -> Response<Body> {
    let method = req.method();
    if method != Method::GET && method != Method::HEAD {
        let mut resp = response::text(
            StatusCode::METHOD_NOT_ALLOWED,
            "proxlet: method not allowed",
        );
        resp.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return resp;
    }
    let head = method == Method::HEAD;
    let path = req.uri().path();
    let rest = if prefix == "/" {
        path
    } else {
        &path[prefix.len()..]
    };

    let Some(candidate) = resolve(&target.root, rest, settings.static_dotfiles) else {
        return not_found();
    };
    // Canonicalising follows symlinks; the result must still be inside the root.
    let Ok(real) = tokio::fs::canonicalize(&candidate).await else {
        return not_found();
    };
    if !real.starts_with(&target.root) {
        return not_found();
    }
    let Ok(meta) = tokio::fs::metadata(&real).await else {
        return not_found();
    };

    if meta.is_dir() {
        if !path.ends_with('/') {
            return redirect_to_slash(req);
        }
        let index = real.join("index.html");
        if let Ok(index_meta) = tokio::fs::metadata(&index).await
            && index_meta.is_file()
        {
            return send_file(&index, &index_meta, req.headers(), head).await;
        }
        if settings.static_listing {
            return listing(&real, path, settings.static_dotfiles, head).await;
        }
        return not_found();
    }
    if meta.is_file() {
        return send_file(&real, &meta, req.headers(), head).await;
    }
    not_found()
}

fn not_found() -> Response<Body> {
    response::text(StatusCode::NOT_FOUND, "proxlet: not found")
}

/// Maps the request path below the rule prefix to a path under `root`.
///
/// Returns `None` for anything that could step outside the root (`..`,
/// encoded separators, NUL) or names a dotfile while dotfiles are hidden.
fn resolve(root: &Path, rest: &str, dotfiles: bool) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for raw in rest.split('/').filter(|s| !s.is_empty()) {
        let bytes: Vec<u8> = percent_decode_str(raw).collect();
        if bytes.is_empty()
            || bytes == b"."
            || bytes == b".."
            || bytes.iter().any(|b| matches!(b, b'/' | b'\\' | 0))
        {
            return None;
        }
        if !dotfiles && bytes[0] == b'.' {
            return None;
        }
        path.push(OsStr::from_bytes(&bytes));
    }
    Some(path)
}

fn redirect_to_slash(req: &Request<Incoming>) -> Response<Body> {
    let mut location = format!("{}/", req.uri().path());
    if let Some(q) = req.uri().query() {
        location.push('?');
        location.push_str(q);
    }
    let mut resp = response::status(StatusCode::MOVED_PERMANENTLY);
    if let Ok(v) = HeaderValue::from_str(&location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

fn etag(meta: &std::fs::Metadata) -> String {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    format!(
        "\"{:x}-{:x}-{:x}\"",
        mtime.as_secs(),
        mtime.subsec_nanos(),
        meta.len()
    )
}

fn not_modified(headers: &HeaderMap, tag: &str, modified: Option<SystemTime>) -> bool {
    if let Some(inm) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        return inm
            .split(',')
            .map(str::trim)
            .any(|t| t == "*" || t.trim_start_matches("W/") == tag);
    }
    let since = headers
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| httpdate::parse_http_date(v).ok());
    match (since, modified) {
        (Some(since), Some(modified)) => {
            let secs = |t: SystemTime| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).ok();
            secs(modified) <= secs(since)
        }
        _ => false,
    }
}

/// A satisfiable single byte range, inclusive.
#[derive(Debug, PartialEq, Eq)]
enum Range {
    Full,
    Partial(u64, u64),
    Unsatisfiable,
}

fn parse_range(value: Option<&str>, len: u64) -> Range {
    let Some(spec) = value.and_then(|v| v.trim().strip_prefix("bytes=")) else {
        return Range::Full;
    };
    if spec.contains(',') {
        // Multiple ranges are allowed to be answered with the full body.
        return Range::Full;
    }
    let Some((start, end)) = spec.trim().split_once('-') else {
        return Range::Full;
    };
    let (start, end) = match (start.parse::<u64>().ok(), end.parse::<u64>().ok()) {
        (Some(s), Some(e)) if s <= e => (s, e.min(len.saturating_sub(1))),
        (Some(s), None) if end.is_empty() => (s, len.saturating_sub(1)),
        (None, Some(n)) if start.is_empty() && n > 0 => {
            (len.saturating_sub(n), len.saturating_sub(1))
        }
        _ => return Range::Full,
    };
    if len == 0 || start >= len {
        Range::Unsatisfiable
    } else {
        Range::Partial(start, end)
    }
}

async fn send_file(
    path: &Path,
    meta: &std::fs::Metadata,
    headers: &HeaderMap,
    head: bool,
) -> Response<Body> {
    let len = meta.len();
    let tag = etag(meta);
    let modified = meta.modified().ok();

    let mut resp = response::status(StatusCode::OK);
    {
        let h = resp.headers_mut();
        if let Ok(v) = HeaderValue::from_str(&tag) {
            h.insert(header::ETAG, v);
        }
        if let Some(m) = modified
            && let Ok(v) = HeaderValue::from_str(&httpdate::fmt_http_date(m))
        {
            h.insert(header::LAST_MODIFIED, v);
        }
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }

    if not_modified(headers, &tag, modified) {
        *resp.status_mut() = StatusCode::NOT_MODIFIED;
        return resp;
    }

    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let content_type = if mime.type_() == mime_guess::mime::TEXT
        || mime.essence_str() == "application/javascript"
        || mime.essence_str() == "application/json"
    {
        format!("{}; charset=utf-8", mime.essence_str())
    } else {
        mime.essence_str().to_owned()
    };
    if let Ok(v) = HeaderValue::from_str(&content_type) {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }

    let if_range_ok = headers
        .get(header::IF_RANGE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v.trim() == tag);
    let range = if if_range_ok {
        parse_range(
            headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
            len,
        )
    } else {
        Range::Full
    };
    let (start, count) = match range {
        Range::Full => (0, len),
        Range::Partial(s, e) => {
            *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
            if let Ok(v) = HeaderValue::from_str(&format!("bytes {s}-{e}/{len}")) {
                resp.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            (s, e - s + 1)
        }
        Range::Unsatisfiable => {
            let mut r = response::status(StatusCode::RANGE_NOT_SATISFIABLE);
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{len}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return r;
        }
    };
    resp.headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from(count));
    if head {
        return resp;
    }

    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return not_found();
    };
    if start > 0 && file.seek(SeekFrom::Start(start)).await.is_err() {
        return response::text(
            StatusCode::INTERNAL_SERVER_ERROR,
            "proxlet: could not read file",
        );
    }
    let stream = ReaderStream::with_capacity(file.take(count), 64 * 1024)
        .map_ok(Frame::data)
        .map_err(Into::into);
    *resp.body_mut() = BodyExt::boxed(StreamBody::new(stream));
    resp
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

async fn listing(dir: &Path, request_path: &str, dotfiles: bool, head: bool) -> Response<Body> {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return not_found();
    };
    let mut items: Vec<(bool, String)> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !dotfiles && name.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().await.is_ok_and(|t| t.is_dir());
        items.push((is_dir, name));
    }
    items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let title = escape_html(&percent_decode_str(request_path).decode_utf8_lossy());
    let mut html = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>Index of {title}</title>\
         <style>body{{font:14px system-ui,sans-serif;margin:2rem}}a{{text-decoration:none}}li{{padding:2px 0}}</style>\
         </head><body><h1>Index of {title}</h1><ul>\n"
    );
    if request_path != "/" {
        html.push_str("<li><a href=\"../\">../</a></li>\n");
    }
    for (is_dir, name) in &items {
        let slash = if *is_dir { "/" } else { "" };
        let href = utf8_percent_encode(name, LINK);
        let _ = writeln!(
            html,
            "<li><a href=\"{href}{slash}\">{}{slash}</a></li>",
            escape_html(name)
        );
    }
    html.push_str("</ul></body></html>\n");

    let mut resp = response::status(StatusCode::OK);
    let h = resp.headers_mut();
    h.insert(header::SERVER, SERVER);
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(html.len()));
    if !head {
        *resp.body_mut() = response::full(Bytes::from(html));
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_rejects_escapes() {
        let root = Path::new("/srv");
        assert_eq!(
            resolve(root, "/a/b.txt", false),
            Some("/srv/a/b.txt".into())
        );
        assert_eq!(resolve(root, "/a%20b", false), Some("/srv/a b".into()));
        for bad in [
            "/../etc",
            "/a/%2e%2e/b",
            "/a%2fb",
            "/a%00b",
            "/a%5cb",
            "/./x",
        ] {
            assert_eq!(resolve(root, bad, true), None, "{bad}");
        }
    }

    #[test]
    fn resolve_hides_dotfiles_unless_enabled() {
        let root = Path::new("/srv");
        assert_eq!(resolve(root, "/.env", false), None);
        assert_eq!(resolve(root, "/.git/config", false), None);
        assert_eq!(resolve(root, "/.env", true), Some("/srv/.env".into()));
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 10), Range::Full);
        assert_eq!(parse_range(Some("bytes=0-3"), 10), Range::Partial(0, 3));
        assert_eq!(parse_range(Some("bytes=5-"), 10), Range::Partial(5, 9));
        assert_eq!(parse_range(Some("bytes=-4"), 10), Range::Partial(6, 9));
        assert_eq!(parse_range(Some("bytes=8-100"), 10), Range::Partial(8, 9));
        assert_eq!(parse_range(Some("bytes=10-"), 10), Range::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-1,4-5"), 10), Range::Full);
        assert_eq!(parse_range(Some("items=0-1"), 10), Range::Full);
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(
            escape_html("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
