use color_eyre::Result;
use http_body_util::BodyExt;
use http_body_util::Empty;
use http_body_util::Full;
use hyper::Method;
use hyper::Request;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;

pub async fn request_json<T: serde::de::DeserializeOwned, R: serde::Serialize>(
    socket_path: &str,
    method: Method,
    path: &str,
    body: Option<R>,
) -> Result<T> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        color_eyre::eyre::eyre!(
            "Failed to connect to dockernatmap daemon at {}: {}\nIs the daemon running?",
            socket_path,
            e
        )
    })?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;

    tokio::task::spawn(async move {
        if let Err(err) = conn.await {
            tracing::error!("Connection failed: {:?}", err);
        }
    });

    let mut req_builder = Request::builder()
        .uri(format!("http://localhost{}", path))
        .method(method)
        .header("Host", "localhost");

    let req = if let Some(b) = body {
        req_builder = req_builder.header("Content-Type", "application/json");
        let json = serde_json::to_vec(&b)?;
        req_builder.body(Full::new(hyper::body::Bytes::from(json)).boxed())?
    } else {
        req_builder.body(Empty::<hyper::body::Bytes>::new().boxed())?
    };

    let res = sender.send_request(req).await?;
    let status = res.status();
    let body_bytes = res.into_body().collect().await?.to_bytes();

    if !status.is_success() {
        let err_msg = String::from_utf8_lossy(&body_bytes);
        color_eyre::eyre::bail!("Daemon returned error {}: {}", status, err_msg);
    }

    if body_bytes.is_empty() {
        return Ok(serde_json::from_str("null")
            .unwrap_or_else(|_| serde_json::from_value(serde_json::Value::Null).unwrap()));
    }

    let parsed: T = serde_json::from_slice(&body_bytes)?;
    Ok(parsed)
}
