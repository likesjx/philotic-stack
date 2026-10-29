//! HTTP handoff stays inside the hotel's result-release reservation.
use axum::body::Bytes;
use std::time::Instant;

#[async_trait::async_trait]
pub(crate) trait Lease: Send + 'static {
    async fn validate(&mut self, digest: [u8; 32]) -> bool;
    async fn finish(self) -> bool;
}
#[async_trait::async_trait]
impl Lease for native_memory::authority_control::Client {
    async fn validate(&mut self, digest: [u8; 32]) -> bool {
        self.validate(digest).await.is_ok()
    }
    async fn finish(self) -> bool {
        self.finish(false).await.is_ok()
    }
}

pub(crate) fn stream<L: Lease>(
    bytes: Bytes,
    lease: L,
    digest: [u8; 32],
    deadline: Instant,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    futures_util::stream::unfold(
        (Some(bytes), Some(lease)),
        move |(bytes, mut lease)| async move {
            if let Some(bytes) = bytes {
                let valid = if Instant::now() < deadline {
                    match lease.as_mut() {
                        Some(l) => l.validate(digest).await,
                        None => false,
                    }
                } else {
                    false
                };
                if !valid || Instant::now() >= deadline {
                    return Some((
                        Err(std::io::Error::other("native release expired")),
                        (None, None),
                    ));
                }
                // No await between final deadline check and handing over this frame.
                return Some((Ok(bytes), (None, lease)));
            }
            if let Some(lease) = lease {
                if !lease.finish().await {
                    return Some((
                        Err(std::io::Error::other(
                            "native release acknowledgement unavailable",
                        )),
                        (None, None),
                    ));
                }
            }
            None
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Fake {
        state: Arc<AtomicUsize>,
        valid: bool,
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.state.fetch_add(100, Ordering::SeqCst);
        }
    }
    #[async_trait::async_trait]
    impl Lease for Fake {
        async fn validate(&mut self, _: [u8; 32]) -> bool {
            self.state.fetch_add(1, Ordering::SeqCst);
            self.valid
        }
        async fn finish(self) -> bool {
            self.state.fetch_add(10, Ordering::SeqCst);
            true
        }
    }
    #[tokio::test]
    async fn expired_first_poll_or_failed_validation_never_yields_data() {
        for (valid, expired) in [(true, true), (false, false)] {
            let state = Arc::new(AtomicUsize::new(0));
            let deadline = if expired {
                Instant::now()
            } else {
                Instant::now() + std::time::Duration::from_secs(1)
            };
            let stream = stream(
                Bytes::from_static(b"private result"),
                Fake {
                    state: state.clone(),
                    valid,
                },
                [0; 32],
                deadline,
            );
            futures_util::pin_mut!(stream);
            assert!(stream.next().await.unwrap().is_err());
            assert!(stream.next().await.is_none());
            assert_eq!(
                state.load(Ordering::SeqCst),
                if expired { 100 } else { 101 }
            );
        }
    }
    #[tokio::test]
    async fn handoff_retains_lease_until_next_poll_and_drop_never_finishes_it() {
        for finish in [false, true] {
            let state = Arc::new(AtomicUsize::new(0));
            {
                let stream = stream(
                    Bytes::from_static(b"result"),
                    Fake {
                        state: state.clone(),
                        valid: true,
                    },
                    [0; 32],
                    Instant::now() + std::time::Duration::from_secs(1),
                );
                futures_util::pin_mut!(stream);
                assert_eq!(stream.next().await.unwrap().unwrap(), "result");
                assert_eq!(state.load(Ordering::SeqCst), 1);
                if finish {
                    assert!(stream.next().await.is_none());
                }
            }
            assert_eq!(state.load(Ordering::SeqCst), if finish { 111 } else { 101 });
        }
    }
}
