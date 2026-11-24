//! Access records: one per answered request, emitted when its response body
//! ends or is dropped (design decision 2).

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use http::Request;
use hyper::body::{Body as HttpBody, Frame, SizeHint};

use crate::stats::InFlight;
use crate::{Body, BoxError, Origin};

/// What the server did with one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRecord {
    /// When the request arrived.
    pub time: SystemTime,
    /// The client's address: the peer, or for a front server the first
    /// `X-Forwarded-For` entry.
    pub client: String,
    /// The request method.
    pub method: String,
    /// The lowercase host without its port, if the request named one.
    pub host: Option<String>,
    /// The path, without the query.
    pub path: String,
    /// The response status.
    pub status: u16,
    /// Body bytes sent to the client.
    pub bytes: u64,
    /// From arrival until the response ended or the client went away.
    pub duration: Duration,
    /// The matched rule's route, if a rule answered.
    pub rule: Option<String>,
    /// The upstream `host:port`, the served directory, or what else answered
    /// (`acme-challenge`, `redirect`).
    pub target: Option<String>,
    /// Why a proxied request failed (502/504).
    pub error: Option<String>,
}

/// Where access records go.
pub trait AccessLog: Send + Sync {
    /// Takes one record. It runs on the request path, so it must not block.
    fn record(&self, record: AccessRecord);
}

/// Why the upstream could not answer; set as a response extension by the
/// proxy and moved into the record.
#[derive(Debug, Clone)]
pub(crate) struct UpstreamError(pub String);

/// Who answered a request, as the record names it, and the matched rule's
/// in-flight count (statistics).
#[derive(Debug, Default)]
pub(crate) struct Served {
    pub rule: Option<String>,
    pub target: Option<String>,
    pub in_flight: Option<InFlight>,
}

impl Served {
    pub(crate) fn other(target: &str) -> Self {
        Self {
            rule: None,
            target: Some(target.to_owned()),
            in_flight: None,
        }
    }
}

/// The request facts, taken before the request is consumed.
pub(crate) struct Pending {
    sink: Arc<dyn AccessLog>,
    start: Instant,
    record: AccessRecord,
}

impl Pending {
    pub(crate) fn start<B>(
        sink: Arc<dyn AccessLog>,
        peer: SocketAddr,
        origin: Origin,
        host: Option<&str>,
        req: &Request<B>,
    ) -> Self {
        Self {
            sink,
            start: Instant::now(),
            record: AccessRecord {
                time: SystemTime::now(),
                client: client(peer, origin, req),
                method: req.method().as_str().to_owned(),
                host: host.map(|h| crate::host_name(h).to_ascii_lowercase()),
                path: req.uri().path().to_owned(),
                status: 0,
                bytes: 0,
                duration: Duration::ZERO,
                rule: None,
                target: None,
                error: None,
            },
        }
    }
}

/// Wraps the response body so that bytes sent are counted in the rule's
/// statistics, the in-flight count ends with the body, and the access record
/// (when logging) is emitted when it ends.
pub(crate) fn wrap(
    resp: http::Response<Body>,
    served: Served,
    in_flight: InFlight,
    pending: Option<Pending>,
) -> http::Response<Body> {
    let (mut parts, body) = resp.into_parts();
    let error = parts.extensions.remove::<UpstreamError>().map(|e| e.0);
    let pending = pending.map(|mut p| {
        p.record.status = parts.status.as_u16();
        p.record.rule = served.rule;
        p.record.target = served.target;
        p.record.error = error;
        p
    });
    let counted = Counted {
        inner: body,
        pending,
        in_flight,
    };
    http::Response::from_parts(parts, http_body_util::BodyExt::boxed(counted))
}

/// Whether proxlet itself failed to reach the upstream (502/504).
pub(crate) fn upstream_failed(resp: &http::Response<Body>) -> bool {
    matches!(resp.status().as_u16(), 502 | 504)
        && resp.extensions().get::<UpstreamError>().is_some()
}

fn client<B>(peer: SocketAddr, origin: Origin, req: &Request<B>) -> String {
    if origin == Origin::Front
        && let Some(first) = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    {
        return first.to_owned();
    }
    peer.ip().to_canonical().to_string()
}

/// A response body that counts what it sends and emits the access record
/// once, when it is dropped: after the last frame, or when the client left.
struct Counted {
    inner: Body,
    pending: Option<Pending>,
    in_flight: InFlight,
}

impl HttpBody for Counted {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let polled = Pin::new(&mut self.inner).poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &polled
            && let Some(data) = frame.data_ref()
        {
            let n = data.len() as u64;
            self.in_flight.0.record_bytes(n);
            if let Some(p) = self.pending.as_mut() {
                p.record.bytes += n;
            }
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        if let Some(Pending {
            sink,
            start,
            mut record,
        }) = self.pending.take()
        {
            record.duration = start.elapsed();
            sink.record(record);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_is_peer_or_first_forwarded_entry() {
        let peer: SocketAddr = "[::ffff:127.0.0.1]:5000".parse().unwrap();
        let req = Request::get("/")
            .header("x-forwarded-for", " 203.0.113.7 , 10.0.0.1")
            .body(())
            .unwrap();
        assert_eq!(client(peer, Origin::Front, &req), "203.0.113.7");
        assert_eq!(client(peer, Origin::Direct, &req), "127.0.0.1");
        let bare = Request::get("/").body(()).unwrap();
        assert_eq!(client(peer, Origin::Front, &bare), "127.0.0.1");
    }
}
