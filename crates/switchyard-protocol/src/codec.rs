//! Newline-delimited JSON framing with a hard per-frame size limit.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame exceeds {max} bytes")]
    TooLarge { max: usize },
    #[error("connection closed in the middle of a frame")]
    UnexpectedEof,
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid frame JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Reads one non-empty line (without the trailing `\n` / `\r\n`).
///
/// Returns `Ok(None)` on a clean end of stream. Blank lines are skipped.
/// Never buffers more than `max` bytes for a single line.
pub async fn read_line<R>(reader: &mut R, max: usize) -> Result<Option<Vec<u8>>, FrameError>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.iter().all(u8::is_ascii_whitespace) {
                Ok(None)
            } else {
                Err(FrameError::UnexpectedEof)
            };
        }
        let (chunk_len, found_newline) = match available.iter().position(|b| *b == b'\n') {
            Some(i) => (i, true),
            None => (available.len(), false),
        };
        if line.len() + chunk_len > max {
            return Err(FrameError::TooLarge { max });
        }
        line.extend_from_slice(&available[..chunk_len]);
        reader.consume(chunk_len + usize::from(found_newline));
        if found_newline {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                line.clear();
                continue;
            }
            return Ok(Some(line));
        }
    }
}

/// Reads and decodes one frame.
pub async fn read_frame<R, T>(reader: &mut R, max: usize) -> Result<Option<T>, FrameError>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    match read_line(reader, max).await? {
        Some(line) => Ok(Some(serde_json::from_slice(&line)?)),
        None => Ok(None),
    }
}

/// Encodes `frame` as a single line and flushes it.
pub async fn write_frame<W, T>(writer: &mut W, frame: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut bytes = serde_json::to_vec(frame)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::*;

    #[tokio::test]
    async fn reads_lines_and_skips_blank_lines() {
        let mut r = BufReader::with_capacity(4, &b"{\"a\":1}\r\n\n  \n{\"b\":2}\n"[..]);
        assert_eq!(read_line(&mut r, 64).await.unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(read_line(&mut r, 64).await.unwrap().unwrap(), b"{\"b\":2}");
        assert!(read_line(&mut r, 64).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_oversized_frames() {
        let mut r = BufReader::with_capacity(4, &b"0123456789\n"[..]);
        assert!(matches!(read_line(&mut r, 5).await, Err(FrameError::TooLarge { max: 5 })));
    }

    #[tokio::test]
    async fn truncated_frame_is_an_error() {
        let mut r = BufReader::new(&b"{\"a\":"[..]);
        assert!(matches!(read_line(&mut r, 64).await, Err(FrameError::UnexpectedEof)));
    }

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &serde_json::json!({"x": "y\nz"})).await.unwrap();
        assert_eq!(buf.iter().filter(|b| **b == b'\n').count(), 1, "payload newlines are escaped");
        let mut r = BufReader::new(&buf[..]);
        let v: serde_json::Value = read_frame(&mut r, 64).await.unwrap().unwrap();
        assert_eq!(v["x"], "y\nz");
    }
}
