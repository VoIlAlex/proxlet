//! Response helpers for responses proxlet generates itself.

use bytes::Bytes;
use http::{HeaderValue, Response, StatusCode, header};
use http_body_util::{BodyExt, Empty, Full};

use crate::Body;

/// The `Server` header value on responses proxlet generates.
pub(crate) const SERVER: HeaderValue = HeaderValue::from_static("proxlet");

/// An empty body.
pub(crate) fn empty() -> Body {
    Empty::new().map_err(|never| match never {}).boxed()
}

/// A body holding `bytes`.
pub(crate) fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed()
}

/// A plain-text response, the shape of every error proxlet produces.
pub(crate) fn text(status: StatusCode, message: impl Into<String>) -> Response<Body> {
    let mut body = message.into();
    body.push('\n');
    let mut resp = Response::new(full(body));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(header::SERVER, SERVER);
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    resp
}

/// A response with no body and the given status.
pub(crate) fn status(status: StatusCode) -> Response<Body> {
    let mut resp = Response::new(empty());
    *resp.status_mut() = status;
    resp.headers_mut().insert(header::SERVER, SERVER);
    resp
}

/// The response for a request no rule matches.
pub(crate) fn no_rule(host: Option<&str>, path: &str) -> Response<Body> {
    let host = host.unwrap_or("(no host)");
    text(
        StatusCode::NOT_FOUND,
        format!("proxlet: no rule for {host}{path}"),
    )
}
