//! Reverse proxying to an upstream.

use std::error::Error as _;
use std::net::SocketAddr;
use std::time::Duration;

use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use http::{Request, Response, StatusCode, Uri, Version};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};

use crate::response;
use crate::snapshot::ProxyTarget;
use crate::{Body, Origin};

/// The pooled client used for every upstream.
pub(crate) type UpstreamClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Body>;

/// Builds the upstream client with the given connect timeout.
pub(crate) fn client(connect_timeout: Duration) -> UpstreamClient {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_nodelay(true);
    http.set_connect_timeout(Some(connect_timeout));
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .wrap_connector(http);
    Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(90))
        .build(https)
}

/// Headers that describe one connection and never cross a proxy.
const HOP_BY_HOP: [HeaderName; 7] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    HeaderName::from_static("proxy-connection"),
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
];

fn wants_upgrade(headers: &HeaderMap) -> bool {
    headers.contains_key(header::UPGRADE)
        && headers
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
}

/// Removes hop-by-hop headers, including any named in `Connection`. When
/// `keep_upgrade` is set, the upgrade handshake headers are kept.
fn strip_hop_by_hop(headers: &mut HeaderMap, keep_upgrade: bool) {
    let listed: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|t| HeaderName::from_bytes(t.trim().as_bytes()).ok())
        .collect();
    for name in listed.iter().chain(HOP_BY_HOP.iter()) {
        if keep_upgrade && name == header::UPGRADE {
            continue;
        }
        headers.remove(name);
    }
    if keep_upgrade {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    } else {
        headers.remove(header::UPGRADE);
    }
}

fn set_forwarded(headers: &mut HeaderMap, peer: SocketAddr, origin: Origin, host: Option<&str>) {
    if origin == Origin::Front && headers.contains_key("x-forwarded-for") {
        // The front server already recorded the client; the loopback hop adds nothing.
        if !headers.contains_key("x-forwarded-proto") {
            headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
        }
        if !headers.contains_key("x-forwarded-host")
            && let Some(v) = host.and_then(|h| HeaderValue::from_str(h).ok())
        {
            headers.insert("x-forwarded-host", v);
        }
        return;
    }
    let ip = peer.ip().to_canonical().to_string();
    let xff = match headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        Some(prev) if !prev.trim().is_empty() => format!("{prev}, {ip}"),
        _ => ip,
    };
    if let Ok(v) = HeaderValue::from_str(&xff) {
        headers.insert("x-forwarded-for", v);
    }
    let proto = if origin == Origin::Tls {
        "https"
    } else {
        "http"
    };
    headers.insert("x-forwarded-proto", HeaderValue::from_static(proto));
    if let Some(v) = host.and_then(|h| HeaderValue::from_str(h).ok()) {
        headers.insert("x-forwarded-host", v);
    }
}

/// Describes why an upstream could not be reached, in a few words.
fn reason(err: &hyper_util::client::legacy::Error) -> String {
    let mut source = err.source();
    while let Some(e) = source {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            return match io.kind() {
                std::io::ErrorKind::ConnectionRefused => "connection refused".into(),
                std::io::ErrorKind::TimedOut => "timed out connecting".into(),
                _ => io.to_string(),
            };
        }
        if e.source().is_none() {
            return e.to_string();
        }
        source = e.source();
    }
    err.to_string()
}

/// Forwards `req` to the upstream and returns its response, or a 502/504.
pub(crate) async fn forward(
    client: UpstreamClient,
    timeout: Duration,
    target: &ProxyTarget,
    peer: SocketAddr,
    origin: Origin,
    host: Option<String>,
    mut req: Request<Incoming>,
) -> Response<Body> {
    let upgrade = wants_upgrade(req.headers());
    let client_side = upgrade.then(|| hyper::upgrade::on(&mut req));

    let (mut parts, body) = req.into_parts();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map_or("/", http::uri::PathAndQuery::as_str);
    let uri = Uri::builder()
        .scheme(target.scheme.clone())
        .authority(target.authority.clone())
        .path_and_query(path_and_query)
        .build();
    let Ok(uri) = uri else {
        return response::text(StatusCode::BAD_REQUEST, "proxlet: invalid request target");
    };
    parts.uri = uri;
    parts.version = Version::HTTP_11;
    strip_hop_by_hop(&mut parts.headers, upgrade);
    if !parts.headers.contains_key(header::HOST)
        && let Some(v) = host.as_deref().and_then(|h| HeaderValue::from_str(h).ok())
    {
        parts.headers.insert(header::HOST, v);
    }
    set_forwarded(&mut parts.headers, peer, origin, host.as_deref());
    let upstream_req = Request::from_parts(parts, body.map_err(Into::into).boxed());

    let upstream = target.authority.as_str();
    let result = tokio::time::timeout(timeout, client.request(upstream_req)).await;
    let mut resp = match result {
        Err(_) => {
            return response::text(
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "proxlet: {} got no response from {upstream} within {}s",
                    target.route,
                    timeout.as_secs_f32()
                ),
            );
        }
        Ok(Err(e)) => {
            return response::text(
                StatusCode::BAD_GATEWAY,
                format!(
                    "proxlet: {} could not reach {upstream} ({})",
                    target.route,
                    reason(&e)
                ),
            );
        }
        Ok(Ok(resp)) => resp,
    };

    if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        if let Some(client_side) = client_side {
            let upstream_side = hyper::upgrade::on(&mut resp);
            tokio::spawn(async move {
                if let (Ok(a), Ok(b)) = tokio::join!(client_side, upstream_side) {
                    let mut a = TokioIo::new(a);
                    let mut b = TokioIo::new(b);
                    let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
                }
            });
        }
        let (parts, _) = resp.into_parts();
        return Response::from_parts(parts, response::empty());
    }

    strip_hop_by_hop(resp.headers_mut(), false);
    resp.map(|b| b.map_err(Into::into).boxed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_hop_by_hop_and_connection_listed_headers() {
        let mut h = HeaderMap::new();
        h.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, x-secret"),
        );
        h.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        h.insert("x-secret", HeaderValue::from_static("1"));
        h.insert(
            header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        h.insert("x-keep", HeaderValue::from_static("1"));
        strip_hop_by_hop(&mut h, false);
        let names: Vec<&str> = h.keys().map(HeaderName::as_str).collect();
        assert_eq!(names, ["x-keep"]);
    }

    #[test]
    fn keeps_upgrade_handshake() {
        let mut h = HeaderMap::new();
        h.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
        h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        assert!(wants_upgrade(&h));
        strip_hop_by_hop(&mut h, true);
        assert_eq!(h[header::CONNECTION], "upgrade");
        assert_eq!(h[header::UPGRADE], "websocket");
    }

    #[test]
    fn forwarded_for_appends() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.1"));
        set_forwarded(
            &mut h,
            "203.0.113.7:5000".parse().unwrap(),
            Origin::Direct,
            Some("example.com"),
        );
        assert_eq!(h["x-forwarded-for"], "10.0.0.1, 203.0.113.7");
        assert_eq!(h["x-forwarded-proto"], "http");
        assert_eq!(h["x-forwarded-host"], "example.com");
    }

    #[test]
    fn front_server_headers_are_kept() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.7"));
        h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        set_forwarded(
            &mut h,
            "127.0.0.1:5000".parse().unwrap(),
            Origin::Front,
            Some("example.com"),
        );
        assert_eq!(h["x-forwarded-for"], "203.0.113.7");
        assert_eq!(h["x-forwarded-proto"], "https");
        assert_eq!(h["x-forwarded-host"], "example.com");
    }

    #[test]
    fn ipv4_mapped_peer_is_shown_as_ipv4() {
        let mut h = HeaderMap::new();
        set_forwarded(
            &mut h,
            "[::ffff:203.0.113.7]:5000".parse().unwrap(),
            Origin::Direct,
            None,
        );
        assert_eq!(h["x-forwarded-for"], "203.0.113.7");
    }
}
