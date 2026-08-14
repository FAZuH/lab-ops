//! HTTP client helpers for communicating with the natmap daemon over its Unix socket.

use std::fmt;
use std::path::Path;

use http_body_util::BodyExt;
use http_body_util::Empty;
use http_body_util::Full;
use hyper::Method;
use hyper::Request;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;

/// Error type for the natmap daemon client.
///
/// Distinct from `color_eyre::Result` so callers can match on daemon status
/// codes without parsing error strings. Transport/parse failures carry their
/// message; daemon rejections are mapped to the status code they arrived on.
#[derive(Debug)]
pub enum NatmapError {
    /// Failed to connect to the daemon's Unix socket.
    Connect(String),
    /// The HTTP exchange with the daemon failed (handshake, send, or read).
    Http(String),
    /// The daemon's JSON response could not be deserialized.
    Json(String),
    /// The daemon rejected the request (400 Bad Request).
    BadRequest(String),
    /// The requested resource does not exist (404 Not Found).
    NotFound(String),
    /// The request conflicts with existing state, e.g. a port is already
    /// allocated (409 Conflict).
    Conflict(String),
    /// The daemon failed internally (500 Internal Server Error).
    Internal(String),
    /// A required backend (Docker) is unavailable in the daemon
    /// (503 Service Unavailable).
    Unavailable(String),
    /// Any non-success status outside the known set.
    UnexpectedStatus { status: u16, body: String },
}

impl fmt::Display for NatmapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(msg) => write!(f, "failed to connect to natmap daemon: {msg}"),
            Self::Http(msg) => write!(f, "natmap HTTP error: {msg}"),
            Self::Json(msg) => write!(f, "natmap response parse error: {msg}"),
            Self::BadRequest(msg) => write!(f, "natmap rejected request: {msg}"),
            Self::NotFound(msg) => write!(f, "natmap: not found: {msg}"),
            Self::Conflict(msg) => write!(f, "natmap: conflict: {msg}"),
            Self::Internal(msg) => write!(f, "natmap daemon error: {msg}"),
            Self::Unavailable(msg) => write!(f, "natmap: service unavailable: {msg}"),
            Self::UnexpectedStatus { status, body } => {
                write!(f, "natmap returned unexpected status {status}: {body}")
            }
        }
    }
}

impl std::error::Error for NatmapError {}

/// Maps a daemon HTTP status code to the matching [`NatmapError`] variant.
fn from_status(status: hyper::StatusCode, body: String) -> NatmapError {
    match status.as_u16() {
        400 => NatmapError::BadRequest(body),
        404 => NatmapError::NotFound(body),
        409 => NatmapError::Conflict(body),
        500 => NatmapError::Internal(body),
        503 => NatmapError::Unavailable(body),
        code => NatmapError::UnexpectedStatus { status: code, body },
    }
}

/// Sends an HTTP request to the natmap daemon over its Unix socket and deserializes the JSON response.
///
/// Generic over `T` (response type, must implement `DeserializeOwned`) and `R`
/// (request body type, must implement `Serialize`). Pass `None` for `body` on
/// GET and DELETE requests.
///
/// # Errors
///
/// Returns a [`NatmapError`] if the daemon is unreachable, returns a non-success
/// status code, or if JSON deserialization fails.
pub async fn request_json<T: serde::de::DeserializeOwned, R: serde::Serialize>(
    socket_path: impl AsRef<Path>,
    method: Method,
    path: &str,
    body: Option<R>,
) -> Result<T, NatmapError> {
    let socket_path = socket_path.as_ref();
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        NatmapError::Connect(format!(
            "{}: {e}\nIs the daemon running?",
            socket_path.to_string_lossy()
        ))
    })?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| NatmapError::Http(e.to_string()))?;

    tokio::task::spawn(async move {
        if let Err(err) = conn.await {
            tracing::error!("Connection failed: {:?}", err);
        }
    });

    let mut req_builder = Request::builder()
        .uri(format!("http://localhost{path}"))
        .method(method)
        .header("Host", "localhost");

    let req = if let Some(b) = body {
        req_builder = req_builder.header("Content-Type", "application/json");
        let json = serde_json::to_vec(&b).map_err(|e| NatmapError::Json(e.to_string()))?;
        req_builder
            .body(Full::new(hyper::body::Bytes::from(json)).boxed())
            .map_err(|e| NatmapError::Http(e.to_string()))?
    } else {
        req_builder
            .body(Empty::<hyper::body::Bytes>::new().boxed())
            .map_err(|e| NatmapError::Http(e.to_string()))?
    };

    let res = sender
        .send_request(req)
        .await
        .map_err(|e| NatmapError::Http(e.to_string()))?;
    let status = res.status();
    let body_bytes = res
        .into_body()
        .collect()
        .await
        .map_err(|e| NatmapError::Http(e.to_string()))?
        .to_bytes();

    if !status.is_success() {
        let err_msg = String::from_utf8_lossy(&body_bytes).to_string();
        return Err(from_status(status, err_msg));
    }

    if body_bytes.is_empty() {
        return serde_json::from_value(serde_json::Value::Null)
            .map_err(|e| NatmapError::Json(e.to_string()));
    }

    let parsed: T =
        serde_json::from_slice(&body_bytes).map_err(|e| NatmapError::Json(e.to_string()))?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hyper::StatusCode;
    use hyper_util::rt::TokioExecutor;
    use hyper_util::rt::TokioIo;
    use hyper_util::server::conn::auto::Builder;
    use tokio::net::UnixListener;

    use super::*;

    /// Serves a fixed HTTP status + body over a Unix socket and returns the
    /// socket path to point `request_json` at.
    async fn spawn_status_server(
        status: StatusCode,
        body: &'static str,
    ) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("status-test.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let io = TokioIo::new(stream);
                let srv = hyper::service::service_fn(
                    move |_req: hyper::Request<hyper::body::Incoming>| {
                        let body = http_body_util::Full::new(hyper::body::Bytes::from_static(
                            body.as_bytes(),
                        ))
                        .boxed();
                        let response = hyper::Response::builder()
                            .status(status)
                            .body(body)
                            .unwrap();
                        std::future::ready(Ok::<_, std::convert::Infallible>(response))
                    },
                );
                let _ = Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(io, srv)
                    .await;
            }
        });

        (dir, socket_path)
    }

    async fn assert_maps_to(
        status: StatusCode,
        body: &'static str,
        expected: fn(NatmapError) -> bool,
    ) {
        let (_dir, socket) = spawn_status_server(status, body).await;
        let result: Result<(), NatmapError> =
            request_json(socket, Method::GET, "/test", None::<()>).await;
        assert!(expected(result.unwrap_err()));
    }

    #[tokio::test]
    async fn maps_bad_request_status() {
        assert_maps_to(StatusCode::BAD_REQUEST, "bad", |e| {
            matches!(e, NatmapError::BadRequest(_))
        })
        .await;
    }

    #[tokio::test]
    async fn maps_not_found_status() {
        assert_maps_to(StatusCode::NOT_FOUND, "missing", |e| {
            matches!(e, NatmapError::NotFound(_))
        })
        .await;
    }

    #[tokio::test]
    async fn maps_conflict_status() {
        assert_maps_to(StatusCode::CONFLICT, "taken", |e| {
            matches!(e, NatmapError::Conflict(_))
        })
        .await;
    }

    #[tokio::test]
    async fn maps_internal_status() {
        assert_maps_to(StatusCode::INTERNAL_SERVER_ERROR, "boom", |e| {
            matches!(e, NatmapError::Internal(_))
        })
        .await;
    }

    #[tokio::test]
    async fn maps_service_unavailable_status() {
        assert_maps_to(StatusCode::SERVICE_UNAVAILABLE, "no docker", |e| {
            matches!(e, NatmapError::Unavailable(_))
        })
        .await;
    }

    #[tokio::test]
    async fn maps_unknown_status_to_unexpected() {
        let (_dir, socket) = spawn_status_server(StatusCode::IM_A_TEAPOT, "teapot").await;
        let result: Result<(), NatmapError> =
            request_json(socket, Method::GET, "/test", None::<()>).await;
        match result.unwrap_err() {
            NatmapError::UnexpectedStatus { status, body } => {
                assert_eq!(status, 418);
                assert_eq!(body, "teapot");
            }
            other => panic!("expected UnexpectedStatus, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn carries_error_body_in_variant() {
        let (_dir, socket) =
            spawn_status_server(StatusCode::NOT_FOUND, "Container not found").await;
        let result: Result<(), NatmapError> =
            request_json(socket, Method::GET, "/test", None::<()>).await;
        match result.unwrap_err() {
            NatmapError::NotFound(body) => assert_eq!(body, "Container not found"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn connect_error_maps_to_connect_variant() {
        let result: Result<(), NatmapError> =
            request_json("/nonexistent/natmap.sock", Method::GET, "/test", None::<()>).await;
        assert!(matches!(result, Err(NatmapError::Connect(_))));
    }
}
