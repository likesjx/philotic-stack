//! Private child↔hotel control channel. Never place these frames on task routing.
//! No Debug implementation: Admit contains a sensitive short-lived OAuth token.
use crate::{Envelope, Error, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{os::unix::fs::MetadataExt, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Admit {
        guest: String,
        token: String,
        envelope: Box<Envelope>,
    },
    Reserve {
        guest: String,
        handle: String,
        digest: [u8; 32],
    },
    Release {
        guest: String,
        handle: String,
        digest: [u8; 32],
    },
    Cancel {
        guest: String,
        handle: String,
    },
    Validate {
        digest: [u8; 32],
    },
    Finish {
        committed: bool,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Permit { handle: String },
    Ready,
    Finished,
    Cancelled { committed: bool },
    Denied,
}
pub async fn write<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::Invalid)?;
    if bytes.len() > 131072 {
        return Err(Error::Invalid);
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        stream.write_u32(bytes.len() as u32).await?;
        stream.write_all(&bytes).await
    })
    .await
    .map_err(|_| Error::Unavailable)?
    .map_err(|_| Error::Unavailable)
}
pub async fn read<T: DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let size = stream.read_u32().await.map_err(|_| Error::Unavailable)? as usize;
        if size == 0 || size > 131072 {
            return Err(Error::Invalid);
        }
        let mut bytes = vec![0; size];
        stream
            .read_exact(&mut bytes)
            .await
            .map_err(|_| Error::Unavailable)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
    })
    .await
    .map_err(|_| Error::Unavailable)?
}
pub struct Client {
    stream: UnixStream,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HotelControl([REDACTED])")
    }
}
impl Client {
    /// Only a direct hotel-owned child may connect. UID, socket identity and
    /// kernel parent PID are checked independently of any server response.
    pub async fn connect(path: &Path) -> Result<Self> {
        let before = std::fs::symlink_metadata(path).map_err(|_| Error::Unavailable)?;
        let parent = std::fs::symlink_metadata(path.parent().ok_or(Error::Invalid)?)
            .map_err(|_| Error::Unavailable)?;
        use std::os::unix::fs::FileTypeExt;
        let uid = unsafe { libc::geteuid() };
        if !path.is_absolute()
            || !before.file_type().is_socket()
            || before.uid() != uid
            || before.mode() & 0o077 != 0
            || !parent.is_dir()
            || parent.uid() != uid
            || parent.mode() & 0o077 != 0
        {
            return Err(Error::Denied);
        }
        let stream = UnixStream::connect(path)
            .await
            .map_err(|_| Error::Unavailable)?;
        let peer = stream.peer_cred().map_err(|_| Error::Denied)?;
        let after = std::fs::symlink_metadata(path).map_err(|_| Error::Unavailable)?;
        if peer.uid() != uid
            || peer.pid() != Some(unsafe { libc::getppid() })
            || before.dev() != after.dev()
            || before.ino() != after.ino()
        {
            return Err(Error::Denied);
        }
        Ok(Self { stream })
    }
    pub async fn send(&mut self, request: &Request) -> Result<Reply> {
        write(&mut self.stream, request).await?;
        read(&mut self.stream).await
    }
    pub async fn validate(&mut self, digest: [u8; 32]) -> Result<()> {
        match self.send(&Request::Validate { digest }).await? {
            Reply::Ready => Ok(()),
            _ => Err(Error::Denied),
        }
    }
    pub async fn finish(mut self, committed: bool) -> Result<()> {
        match self.send(&Request::Finish { committed }).await? {
            Reply::Finished => Ok(()),
            _ => Err(Error::Uncertain),
        }
    }
}
