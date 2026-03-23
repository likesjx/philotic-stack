use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Maximum allowed frame size (16 MB) to prevent memory exhaustion.
const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

/// Write a length-prefixed bincode frame to an async writer.
pub async fn write_frame<W, T>(writer: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let payload = bincode::serialize(msg).context("failed to serialize message")?;
    let len = payload.len() as u32;
    writer
        .write_all(&len.to_le_bytes())
        .await
        .context("failed to write frame length")?;
    writer
        .write_all(&payload)
        .await
        .context("failed to write frame payload")?;
    writer.flush().await.context("failed to flush writer")?;
    Ok(())
}

/// Read a length-prefixed bincode frame from an async reader.
/// Returns `None` on clean EOF.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: AsyncReadExt + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e).context("failed to read frame length"),
    }

    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME_SIZE {
        anyhow::bail!("frame too large: {} bytes (max {})", len, MAX_FRAME_SIZE);
    }

    let mut payload = vec![0u8; len as usize];
    reader
        .read_exact(&mut payload)
        .await
        .context("failed to read frame payload")?;

    let msg: T = bincode::deserialize(&payload).context("failed to deserialize message")?;
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecuteCommandRequest, ExecuteCommandResponse, ExecutionStatus};
    use std::collections::HashMap;

    #[tokio::test]
    async fn roundtrip_request() {
        let req = ExecuteCommandRequest {
            request_id: "test-123".into(),
            command: "ls".into(),
            args: vec!["-la".into()],
            cwd: Some("/tmp".into()),
            env: HashMap::new(),
            stdin: None,
            timeout_ms: Some(5000),
        };

        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &req).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let decoded: ExecuteCommandRequest = read_frame(&mut cursor).await.unwrap().unwrap();

        assert_eq!(decoded.request_id, "test-123");
        assert_eq!(decoded.command, "ls");
        assert_eq!(decoded.args, vec!["-la"]);
    }

    #[tokio::test]
    async fn roundtrip_response() {
        let resp = ExecuteCommandResponse {
            request_id: "test-456".into(),
            status: ExecutionStatus::Success,
            stdout: b"hello\n".to_vec(),
            stderr: Vec::new(),
            exit_code: Some(0),
            duration_ms: 42,
            error: None,
        };

        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &resp).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let decoded: ExecuteCommandResponse = read_frame(&mut cursor).await.unwrap().unwrap();

        assert_eq!(decoded.request_id, "test-456");
        assert_eq!(decoded.status, ExecutionStatus::Success);
        assert_eq!(decoded.stdout, b"hello\n");
        assert_eq!(decoded.exit_code, Some(0));
    }

    #[tokio::test]
    async fn eof_returns_none() {
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        let result: Option<ExecuteCommandRequest> = read_frame(&mut cursor).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn rejects_oversized_frame() {
        let huge_len: u32 = MAX_FRAME_SIZE + 1;
        let buf = huge_len.to_le_bytes().to_vec();
        let mut cursor = std::io::Cursor::new(buf);
        let result: Result<Option<ExecuteCommandRequest>> = read_frame(&mut cursor).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("frame too large"));
    }
}
