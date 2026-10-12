//! Isolated Linux subprocess fixture for the actual production serve path.
//! Fake encrypted storage only; never invoked by the hotel or a live supervisor.
#[cfg(target_os = "linux")]
fn main() {
    use percival_scoped_vault_tests::{
        domain::{GraphDomain, Record, State},
        scoped_vault::{self, GUEST, Policy, ROLE, Resolver},
        scoped_vault_resolver::HotelVaultResolver,
        vault,
    };
    use std::{
        io::{self, Write},
        os::fd::AsRawFd,
        path::Path,
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tokio::{
        net::UnixListener,
        runtime::Builder,
        signal::unix::{SignalKind, signal},
    };
    use zeroize::Zeroizing;
    struct Fixture {
        inner: HotelVaultResolver,
        stuck: bool,
    }
    impl Resolver for Fixture {
        fn resolve(&self, policy: &Policy) -> io::Result<Zeroizing<String>> {
            assert_eq!(
                policy.secret_ref(),
                "secret://hotel/default/percival-muninn-observe/synthetic"
            );
            println!("synthetic-resolver-entered");
            io::stdout().flush().unwrap();
            if self.stuck {
                loop {
                    std::thread::park();
                }
            }
            self.inner.resolve(policy)
        }
    }
    assert_eq!(
        std::env::var("PERCIVAL_ISOLATED_LINUX_FIXTURE").as_deref(),
        Ok("1")
    );
    let mut args = std::env::args().skip(1);
    let fd: i32 = args
        .next()
        .expect("synthetic inherited FD")
        .parse()
        .unwrap();
    assert!(fd >= 3);
    let stuck = args.next().as_deref() == Some("stuck");
    assert!(args.next().is_none());
    // Exercise the production descriptor validator and adopter, with synthetic
    // supervisor metadata matching this fixture process.
    let adopt = || {
        percival_scoped_vault_tests::scoped_vault_activation::adopt_named(
            &std::process::id().to_string(),
            &(fd - 2).to_string(),
            &(vec!["unrelated"; (fd - 3) as usize]
                .into_iter()
                .chain(std::iter::once("percival-scoped-vault"))
                .collect::<Vec<_>>()
                .join(":")),
        )
    };
    let runtime = Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let inherited = {
        let _entered = runtime.enter();
        match adopt().and_then(|listener| listener.into_std()) {
            Ok(listener) => listener,
            Err(_) => {
                println!("synthetic-startup-rejected");
                std::process::exit(3);
            }
        }
    };
    assert!(inherited.as_raw_fd() != fd);
    let (ciphertext, nonce) = vault::fixture_encrypt("mk_synthetic");
    let graph = GraphDomain(Arc::new(Mutex::new(State {
        record: Record {
            allowed_roles: vec![ROLE.into()],
            allowed_guests: vec![GUEST.into()],
            nonce_b64: nonce,
            ciphertext_b64: ciphertext,
        },
        lookups: vec![],
        replace_after_read: false,
    })));
    let resolver: Arc<dyn Resolver> = Arc::new(Fixture {
        inner: HotelVaultResolver(graph),
        stuck,
    });
    let result:io::Result<()>=runtime.block_on(async {
        let mut term=signal(SignalKind::terminate())?;
        let mut replace=signal(SignalKind::hangup())?;
        loop {
            let listener=UnixListener::from_std(inherited.try_clone()?)?;
            println!("synthetic-listener-generation-started");io::stdout().flush().unwrap();
            tokio::select! {
                result=scoped_vault::serve_from_policy(listener,Path::new(scoped_vault::POLICY_PATH),resolver.clone())=>break result,
                _=term.recv()=>break Ok(()),
                _=replace.recv()=>{}
            }
        }
    });
    // Explicit fixture strategy: bounded wait, followed by full process exit.
    // shutdown_timeout does not kill storage jobs; process exit does.
    runtime.shutdown_timeout(Duration::from_millis(50));
    println!("synthetic-bounded-runtime-shutdown-returned");
    if result.is_err() {
        println!("synthetic-startup-rejected");
        std::process::exit(3);
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux-only synthetic listener fixture");
    std::process::exit(2);
}
