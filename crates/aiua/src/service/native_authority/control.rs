//! Private control adapters owned by the hotel, not a second supervisor.
use super::*;
use native_memory::authority_control::{Reply, Request, read, write};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

impl NativeAuthority {
    pub async fn serve_control(
        self: Arc<Self>,
        listener: UnixListener,
        policy: Arc<dyn Fn() -> Result<native_memory::Policy> + Send + Sync>,
    ) -> Result<()> {
        let capacity = Arc::new(tokio::sync::Semaphore::new(32));
        loop {
            let slot = capacity.clone().acquire_owned().await?;
            let (stream, _) = listener.accept().await?;
            let owner = self.clone();
            let policy = policy.clone();
            tokio::spawn(async move {
                let _slot = slot;
                let outcome = owner.control(stream, policy).await;
                #[cfg(test)]
                eprintln!("synthetic native control completed: {}", outcome.is_ok());
                let _ = outcome;
            });
        }
    }
    async fn control(
        self: Arc<Self>,
        mut stream: UnixStream,
        policy: Arc<dyn Fn() -> Result<native_memory::Policy> + Send + Sync>,
    ) -> Result<()> {
        let first: Request = read(&mut stream).await?;
        let guest = match &first {
            Request::Admit { guest, .. }
            | Request::Reserve { guest, .. }
            | Request::Release { guest, .. }
            | Request::Cancel { guest, .. } => guest,
            _ => return Err(anyhow!("invalid native control start")),
        };
        ensure!(
            [self.membrane.as_str(), self.runner.as_str()].contains(&guest.as_str()),
            "unknown native peer"
        );
        let peer = self.launches.authenticate(&self.hotel, &stream, guest)?;
        match first {
            Request::Admit {
                token, envelope, ..
            } => {
                #[cfg(test)]
                eprintln!("synthetic native control: admit");
                self.origin(&peer)?;
                let verified = native_memory::VerifiedRequest::validate(
                    &envelope,
                    &policy()?,
                    &self.hotel,
                    &self.runner,
                    (self.clock)(),
                )?;
                let handle = self.admit(&peer, token, &verified).await?;
                write(
                    &mut stream,
                    &Reply::Permit {
                        handle: handle.to_string(),
                    },
                )
                .await?;
            }
            Request::Cancel { handle, .. } => {
                let committed = self.cancel(Uuid::parse_str(&handle)?, &peer).await?;
                write(&mut stream, &Reply::Cancelled { committed }).await?;
            }
            request @ (Request::Reserve { .. } | Request::Release { .. }) => {
                let (handle, digest, release) = match request {
                    Request::Reserve { handle, digest, .. } => (handle, digest, false),
                    Request::Release { handle, digest, .. } => (handle, digest, true),
                    _ => unreachable!(),
                };
                #[cfg(test)]
                eprintln!("synthetic native control: reservation release={release}");
                let lease = self
                    .reserve_kind(Uuid::parse_str(&handle)?, peer, digest, release)
                    .await?;
                // Timeout/drop preserves the same journal's dirty state. Never
                // acknowledge clean recovery just because a client disconnected.
                tokio::time::timeout(Duration::from_secs(5), async {
                    write(&mut stream, &Reply::Ready).await?;
                    let mut validations = 0;
                    loop {
                        match read::<Request>(&mut stream).await? {
                            Request::Validate { digest: requested } => {
                                #[cfg(test)]
                                eprintln!("synthetic native control: validate");
                                ensure!(
                                    requested == digest && validations < 2,
                                    "invalid native validation"
                                );
                                validations += 1;
                                lease.validate_before_commit(digest).await?;
                                write(&mut stream, &Reply::Ready).await?;
                            }
                            Request::Finish { committed } => {
                                #[cfg(test)]
                                eprintln!("synthetic native control: finish committed={committed}");
                                ensure!(
                                    release || !committed || validations > 0,
                                    "precommit validation required"
                                );
                                lease.finish(committed).await?;
                                write(&mut stream, &Reply::Finished).await?;
                                return Ok::<(), anyhow::Error>(());
                            }
                            _ => return Err(anyhow!("invalid native reservation frame")),
                        }
                    }
                })
                .await??;
            }
            _ => return Err(anyhow!("invalid native request")),
        }
        Ok(())
    }

    /// Existing Node authority protocol, same journal as native commits. The
    /// issuer guest is fixed by hotel configuration, never supplied by the caller.
    pub async fn serve_issuer(
        self: Arc<Self>,
        listener: UnixListener,
        issuer_guest: String,
        owner_id: String,
    ) -> Result<()> {
        let capacity = Arc::new(tokio::sync::Semaphore::new(16));
        loop {
            let slot = capacity.clone().acquire_owned().await?;
            let (mut stream, _) = listener.accept().await?;
            let owner = self.clone();
            let guest = issuer_guest.clone();
            let id = owner_id.clone();
            tokio::spawn(async move {
                let _slot = slot;
                let _=async {
                    let peer=owner.launches.authenticate(&owner.hotel,&stream,&guest)?;
                    let begin:IssuerFrame=read(&mut stream).await?;
                    ensure!(begin.version==1 && begin.owner==id && begin.sequence==1 && Uuid::parse_str(&begin.session).is_ok() && begin.operation=="begin_issuer_change" && begin.parameters==serde_json::json!({}),"invalid issuer start");
                    let mut fence=owner.fence().await?;
                    tokio::time::timeout(Duration::from_secs(5),async {
                        peer.principal()?;
                        write(&mut stream,&serde_json::json!({"owner":id,"session":begin.session,"sequence":1,"ok":true,"result":true})).await?;
                        let end:IssuerFrame=read(&mut stream).await?;
                        ensure!(end.version==1 && end.owner==id && end.session==begin.session && end.sequence==2 && end.operation=="end_issuer_change" && end.parameters==serde_json::json!({}),"invalid issuer finish");
                        peer.principal()?;fence.finish()?;
                        write(&mut stream,&serde_json::json!({"owner":id,"session":begin.session,"sequence":2,"ok":true,"result":true})).await?;
                        Ok::<(),anyhow::Error>(())
                    }).await??;
                    Ok::<(),anyhow::Error>(())
                }.await;
            });
        }
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IssuerFrame {
    version: u32,
    owner: String,
    session: String,
    sequence: u64,
    operation: String,
    parameters: serde_json::Value,
}
