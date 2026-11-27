//! Serving a snapshot file with HTTP ranges, so a download (or a pull by
//! another server) that breaks off resumes where it stopped instead of
//! starting over at byte 0 of many gigabytes.
//!
//! One range per request (`bytes=a-b`, `bytes=a-`, `bytes=-n`); anything
//! else is answered with the whole file, as RFC 9110 allows.

use crate::error::{AgentError, AgentResult};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::Response;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum Wanted {
    Whole,
    /// First and last byte, inclusive.
    Part(u64, u64),
    /// 416: the range starts past the end.
    Unsatisfiable,
}

/// What to send for `range` (the Range header) of a file of `len` bytes.
/// `if_range` must name this file's ETag, or the whole file is sent: the
/// bytes the client holds belong to another version.
pub fn wanted(range: Option<&str>, if_range: Option<&str>, etag: &str, len: u64) -> Wanted {
    let Some(range) = range else { return Wanted::Whole };
    if let Some(ir) = if_range {
        if ir.trim() != etag {
            return Wanted::Whole;
        }
    }
    let Some(spec) = range.trim().strip_prefix("bytes=") else { return Wanted::Whole };
    if spec.contains(',') || len == 0 {
        return Wanted::Whole;
    }
    let Some((a, b)) = spec.trim().split_once('-') else { return Wanted::Whole };
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() {
        // The last n bytes.
        let Ok(n) = b.parse::<u64>() else { return Wanted::Whole };
        if n == 0 {
            return Wanted::Unsatisfiable;
        }
        return Wanted::Part(len.saturating_sub(n), len - 1);
    }
    let Ok(start) = a.parse::<u64>() else { return Wanted::Whole };
    let end = if b.is_empty() {
        len - 1
    } else {
        match b.parse::<u64>() {
            Ok(e) if e >= start => e.min(len - 1),
            _ => return Wanted::Whole,
        }
    };
    if start >= len {
        return Wanted::Unsatisfiable;
    }
    Wanted::Part(start, end)
}

/// The response for `path`: 200, 206 or 416, with the body left out for
/// HEAD. `etag` is the strong ETag (the file's sha256 in quotes).
pub async fn serve(
    path: &Path,
    method: &Method,
    headers: &HeaderMap,
    etag: &str,
    download_name: &str,
) -> AgentResult<Response> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = tokio::fs::File::open(path).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => AgentError::NotFound,
        _ => AgentError::Io(e),
    })?;
    let len = file.metadata().await?.len();
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let if_range = headers.get(header::IF_RANGE).and_then(|v| v.to_str().ok());
    let base = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, "private, no-store, no-transform")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    let built = |b: axum::http::response::Builder, body: axum::body::Body| {
        b.body(body).map_err(|e| AgentError::Other(anyhow::anyhow!("build snapshot response: {e}")))
    };

    let (status, start, count) = match wanted(range, if_range, etag, len) {
        Wanted::Whole => (StatusCode::OK, 0, len),
        Wanted::Part(a, b) => (StatusCode::PARTIAL_CONTENT, a, b - a + 1),
        Wanted::Unsatisfiable => {
            return built(
                base.status(StatusCode::RANGE_NOT_SATISFIABLE).header(header::CONTENT_RANGE, format!("bytes */{len}")),
                axum::body::Body::empty(),
            );
        }
    };
    let mut b = base
        .status(status)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, count)
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{download_name}\""));
    if status == StatusCode::PARTIAL_CONTENT {
        b = b.header(header::CONTENT_RANGE, format!("bytes {start}-{}/{len}", start + count - 1));
    }
    if method == Method::HEAD {
        return built(b, axum::body::Body::empty());
    }
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut limited = file.take(count);
    let stream = async_stream::stream! {
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            match limited.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => yield Ok::<Vec<u8>, std::io::Error>(buf[..n].to_vec()),
                Err(e) => {
                    yield Err(e);
                    break;
                }
            }
        }
    };
    built(b, axum::body::Body::from_stream(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    const E: &str = "\"abc\"";

    #[test]
    fn ranges() {
        assert_eq!(wanted(None, None, E, 100), Wanted::Whole);
        assert_eq!(wanted(Some("bytes=0-9"), None, E, 100), Wanted::Part(0, 9));
        assert_eq!(wanted(Some("bytes=90-"), None, E, 100), Wanted::Part(90, 99));
        assert_eq!(wanted(Some("bytes=90-500"), None, E, 100), Wanted::Part(90, 99));
        assert_eq!(wanted(Some("bytes=-10"), None, E, 100), Wanted::Part(90, 99));
        assert_eq!(wanted(Some("bytes=-500"), None, E, 100), Wanted::Part(0, 99));
        assert_eq!(wanted(Some("bytes=100-"), None, E, 100), Wanted::Unsatisfiable);
        assert_eq!(wanted(Some("bytes=-0"), None, E, 100), Wanted::Unsatisfiable);
        assert_eq!(wanted(Some("bytes=0-1,5-6"), None, E, 100), Wanted::Whole);
        assert_eq!(wanted(Some("bytes=9-3"), None, E, 100), Wanted::Whole);
        assert_eq!(wanted(Some("items=0-1"), None, E, 100), Wanted::Whole);
        assert_eq!(wanted(Some("bytes=x-1"), None, E, 100), Wanted::Whole);
    }

    #[test]
    fn if_range_must_match_this_version() {
        assert_eq!(wanted(Some("bytes=10-"), Some(E), E, 100), Wanted::Part(10, 99));
        assert_eq!(wanted(Some("bytes=10-"), Some("\"other\""), E, 100), Wanted::Whole);
    }
}
