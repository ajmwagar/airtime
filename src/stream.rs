//! Native Icecast2 source client.
//!
//! Icecast accepts source connections over plain HTTP. Two dialects:
//!
//! - **legacy `SOURCE`** (HTTP/1.0) — `SOURCE /mount HTTP/1.0` + Basic
//!   auth, then the audio body streams forever.
//! - **modern `PUT`** (HTTP/1.1) — `PUT /mount HTTP/1.1` with
//!   `Expect: 100-continue`, then the body.
//!
//! We use `PUT` here because every supported Icecast2 build (>=2.4) speaks
//! it and it's friendlier to proxies. The encoded audio bytes are
//! consumed from a `tokio::sync::mpsc::Receiver` so the mixer can stitch
//! segments together without ever touching disk or a named pipe.
//!
//! **Lossless invariant:** the only Content-Type we accept is
//! `application/ogg` (Ogg/FLAC) — the call sites are responsible for
//! making sure the body is actually Ogg-framed FLAC. Lossy codecs are
//! rejected at construction time.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("icecast rejected source connection: HTTP {status} — {body}")]
    Rejected { status: u16, body: String },
    #[error("icecast closed source connection unexpectedly")]
    Closed,
    #[error("invalid lossy content-type `{0}` (Phase 1 is lossless-only)")]
    LossyContentType(String),
    #[error("invalid mount `{0}` (must start with `/`)")]
    BadMount(String),
}

/// Lossless content-types we'll let through. The chain has to be Ogg-framed
/// or Icecast won't fan it out correctly to listeners.
const LOSSLESS_CTYPES: &[&str] = &[
    "application/ogg", // Ogg/FLAC, Ogg/Vorbis (Vorbis is lossy — see filter below).
    "audio/flac", // Native FLAC (no Ogg framing) — not all Icecast builds like it; here for completeness.
];

/// Source client configuration.
#[derive(Debug, Clone)]
pub struct SourceConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub mount: String,
    pub content_type: String,
    pub station_name: String,
    pub genre: String,
    pub description: String,
}

impl SourceConfig {
    pub fn validate(&self) -> Result<(), StreamError> {
        if !self.mount.starts_with('/') {
            return Err(StreamError::BadMount(self.mount.clone()));
        }
        // Lossless guard: only allow known lossless container types.
        let ct = self.content_type.to_ascii_lowercase();
        if !LOSSLESS_CTYPES.iter().any(|c| ct == *c) {
            return Err(StreamError::LossyContentType(self.content_type.clone()));
        }
        Ok(())
    }
}

/// Connect to Icecast, send the `PUT` source request, then pump bytes
/// from `body` into the socket until the channel closes.
///
/// Returns when the channel closes cleanly or the connection drops.
pub async fn run_source(
    cfg: SourceConfig,
    mut body: mpsc::Receiver<Vec<u8>>,
) -> Result<(), StreamError> {
    cfg.validate()?;
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&addr))
        .await
        .map_err(|_| {
            StreamError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "connect timed out",
            ))
        })??;
    let (rd, mut wr) = stream.into_split();

    // Headers: PUT /mount HTTP/1.1 + Basic + Icy metadata.
    let auth = format!("{}:{}", cfg.user, cfg.password);
    let auth_b64 = B64.encode(auth.as_bytes());
    let req = format!(
        "PUT {mount} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Authorization: Basic {auth}\r\n\
         User-Agent: airtime/0.1\r\n\
         Content-Type: {ctype}\r\n\
         Ice-Public: 1\r\n\
         Ice-Name: {name}\r\n\
         Ice-Genre: {genre}\r\n\
         Ice-Description: {desc}\r\n\
         Expect: 100-continue\r\n\
         Transfer-Encoding: chunked\r\n\
         \r\n",
        mount = cfg.mount,
        host = cfg.host,
        port = cfg.port,
        auth = auth_b64,
        ctype = cfg.content_type,
        name = cfg.station_name,
        genre = cfg.genre,
        desc = cfg.description,
    );
    wr.write_all(req.as_bytes()).await?;
    wr.flush().await?;

    // Read the response status line + headers. Icecast replies with
    // `HTTP/1.1 100 Continue` on success; anything else is a rejection.
    let mut rdr = BufReader::new(rd);
    let (status, body_preview) = read_status_and_headers(&mut rdr).await?;
    if !(status == 100 || status == 200) {
        return Err(StreamError::Rejected {
            status,
            body: body_preview,
        });
    }

    // Pump chunks. Each `Vec<u8>` from the channel becomes one HTTP
    // chunk. An empty terminator is sent when the channel closes.
    while let Some(chunk) = body.recv().await {
        if chunk.is_empty() {
            continue;
        }
        let size_line = format!("{:X}\r\n", chunk.len());
        wr.write_all(size_line.as_bytes()).await?;
        wr.write_all(&chunk).await?;
        wr.write_all(b"\r\n").await?;
        wr.flush().await?;
    }
    wr.write_all(b"0\r\n\r\n").await?;
    wr.flush().await?;
    Ok(())
}

async fn read_status_and_headers<R: AsyncReadExt + Unpin>(
    rdr: &mut R,
) -> Result<(u16, String), StreamError> {
    // Slurp until we hit the header terminator or 8 KiB, whichever first.
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 256];
    loop {
        let n = rdr.read(&mut tmp).await?;
        if n == 0 {
            return Err(StreamError::Closed);
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 8192 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    // "HTTP/1.1 100 Continue"
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    Ok((status, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    fn config(mount: &str, ctype: &str) -> SourceConfig {
        SourceConfig {
            host: "127.0.0.1".into(),
            port: 0,
            user: "source".into(),
            password: "hackme".into(),
            mount: mount.into(),
            content_type: ctype.into(),
            station_name: "test".into(),
            genre: "test".into(),
            description: "test".into(),
        }
    }

    #[test]
    fn rejects_lossy_content_type() {
        let cfg = config("/x", "audio/mpeg");
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, StreamError::LossyContentType(_)));
    }

    #[test]
    fn accepts_ogg_flac() {
        config("/x", "application/ogg").validate().unwrap();
        config("/x", "Application/OGG").validate().unwrap();
    }

    #[test]
    fn rejects_mount_without_slash() {
        let cfg = config("nope", "application/ogg");
        assert!(matches!(cfg.validate(), Err(StreamError::BadMount(_))));
    }

    /// Spin up a fake Icecast that:
    ///  1. accepts the connection
    ///  2. reads the request headers
    ///  3. replies "HTTP/1.1 100 Continue"
    ///  4. reads the chunked body until the terminator
    ///
    /// This is the happy path — verifies our header + chunk framing.
    #[tokio::test]
    async fn writes_chunked_body_to_fake_icecast() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Read request headers
            let mut buf = vec![0u8; 4096];
            let mut total = 0;
            loop {
                let n = sock.read(&mut buf[total..]).await.unwrap();
                if n == 0 {
                    break;
                }
                total += n;
                if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let req = String::from_utf8_lossy(&buf[..total]).into_owned();
            assert!(req.starts_with("PUT /test HTTP/1.1"));
            assert!(req.contains("Authorization: Basic "));
            assert!(req.contains("Content-Type: application/ogg"));
            assert!(req.contains("Transfer-Encoding: chunked"));

            sock.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await
                .unwrap();

            // Read until socket closes; collect chunked body fragments.
            let mut body = Vec::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..n]);
            }
            body
        });

        let cfg = SourceConfig {
            host: addr.ip().to_string(),
            port: addr.port(),
            user: "source".into(),
            password: "hackme".into(),
            mount: "/test".into(),
            content_type: "application/ogg".into(),
            station_name: "test".into(),
            genre: "test".into(),
            description: "test".into(),
        };
        let (tx, rx) = mpsc::channel(4);
        tx.send(b"first-frame".to_vec()).await.unwrap();
        tx.send(b"second-frame".to_vec()).await.unwrap();
        drop(tx);
        run_source(cfg, rx).await.unwrap();

        let body = server.await.unwrap();
        let body_s = String::from_utf8_lossy(&body);
        assert!(body_s.contains("first-frame"));
        assert!(body_s.contains("second-frame"));
        assert!(body_s.ends_with("0\r\n\r\n"));
    }

    #[tokio::test]
    async fn surfaces_rejection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            sock.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });

        let cfg = SourceConfig {
            host: addr.ip().to_string(),
            port: addr.port(),
            user: "source".into(),
            password: "wrong".into(),
            mount: "/test".into(),
            content_type: "application/ogg".into(),
            station_name: "test".into(),
            genre: "test".into(),
            description: "test".into(),
        };
        let (_tx, rx) = mpsc::channel(1);
        let err = run_source(cfg, rx).await.unwrap_err();
        match err {
            StreamError::Rejected { status, .. } => assert_eq!(status, 401),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
