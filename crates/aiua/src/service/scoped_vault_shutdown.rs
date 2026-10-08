//! Independent stop deadline: neither Tokio nor graph/SQLite locks can delay it.
use std::{io, time::Duration};

pub const NORMAL_STOP_LIMIT: Duration = Duration::from_secs(35);
pub const STARTUP_TEST_STOP_LIMIT: Duration = Duration::from_secs(2);

/// Install before runtime/bootstrap: observing the stop signal must not depend
/// on an async worker becoming runnable when the storage mutex is held.
#[cfg(unix)]
pub fn install_signal_limit(enabled: bool, limit: Duration) -> io::Result<()> {
    if !enabled {
        return Ok(());
    }
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let stopping = Arc::new(AtomicBool::new(false));
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let flag = stopping.clone();
        // SAFETY: handler performs only a lock-free atomic store, no allocation,
        // tracing, storage access or runtime interaction.
        unsafe {
            signal_hook_registry::register(signal, move || flag.store(true, Ordering::Relaxed))?;
        }
    }
    std::thread::Builder::new()
        .name("scoped-vault-signal-stop".into())
        .spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(limit);
            std::process::exit(1);
        })?;
    Ok(())
}
#[cfg(not(unix))]
pub fn install_signal_limit(enabled: bool, _limit: Duration) -> io::Result<()> {
    if enabled {
        Err(io::Error::other("scoped vault requires Unix signals"))
    } else {
        Ok(())
    }
}

/// Monitor endpoint completion and panic through the same owner contract.
pub fn supervise(
    work: impl std::future::Future<Output = ()> + Send + 'static,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) {
    let task = tokio::spawn(async move {
        tokio::select! {
            _ = work => false,
            _ = shutdown.recv() => true,
        }
    });
    tokio::spawn(async move {
        if !matches!(task.await, Ok(true)) {
            tracing::error!(
                "Scoped vault endpoint failed or panicked; supervisor restart required"
            );
            std::process::exit(1);
        }
    });
}

/// Arm before any drain/cleanup work. The process must exit before this deadline.
/// A native thread, independent of the async runtime and storage, forces exit on
/// expiry. Successful main return exits the process without waiting for it.
pub fn arm(enabled: bool, limit: Duration) -> io::Result<()> {
    if enabled {
        std::thread::Builder::new()
            .name("scoped-vault-stop-deadline".into())
            .spawn(move || {
                std::thread::sleep(limit);
                std::process::exit(1);
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::{domain::GraphDomain, sqlite_storage::SqliteGraphStorage};
    use std::{
        process::{Command, Stdio},
        sync::{Arc, mpsc},
        time::Instant,
    };

    #[test]
    fn supervised_endpoint_panic_child() {
        if std::env::var("PERCIVAL_SYNTHETIC_ENDPOINT_PANIC_CHILD").as_deref() != Ok("1") {
            return;
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (_tx, rx) = tokio::sync::broadcast::channel::<()>(1);
            supervise(
                async {
                    panic!("synthetic endpoint panic");
                },
                rx,
            );
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        panic!("endpoint panic did not terminate process");
    }

    #[test]
    fn endpoint_panic_forces_process_exit() {
        let began = Instant::now();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "service::scoped_vault_shutdown::tests::supervised_endpoint_panic_child",
                "--nocapture",
            ])
            .env("PERCIVAL_SYNTHETIC_ENDPOINT_PANIC_CHILD", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if began.elapsed() > Duration::from_secs(3) {
                child.kill().unwrap();
                panic!("endpoint panic was not supervised");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = child.wait_with_output().unwrap();
        assert_eq!(status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("synthetic endpoint panic"));
    }

    #[test]
    fn actual_storage_lock_child() {
        let mode = std::env::var("PERCIVAL_SYNTHETIC_STORAGE_STOP_CHILD").unwrap_or_default();
        if mode != "1" && mode != "signal" {
            return;
        }
        if mode == "signal" {
            install_signal_limit(true, Duration::from_millis(200)).unwrap();
        }
        let storage = Arc::new(SqliteGraphStorage::open_in_memory().unwrap());
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let conn = storage.raw_conn().clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _guard = conn.lock().unwrap();
            tx.send(()).unwrap();
            loop {
                std::thread::park();
            }
        });
        rx.recv().unwrap();
        assert!(storage.raw_conn().try_lock().is_err());
        if mode == "1" {
            arm(true, Duration::from_millis(200)).unwrap();
        }
        println!("actual-sqlite-mutex-held-before-pid-cleanup");
        use std::io::Write;
        std::io::stdout().flush().unwrap();
        // The exact production cleanup method blocks on the held shared mutex.
        graph.set_hotel_pid("synthetic-stop-fixture", None).unwrap();
        panic!("shared SQLite cleanup unexpectedly returned");
    }

    #[test]
    fn deadline_exits_process_with_actual_sqlite_mutex_held() {
        let began = Instant::now();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "service::scoped_vault_shutdown::tests::actual_storage_lock_child",
                "--nocapture",
            ])
            .env("PERCIVAL_SYNTHETIC_STORAGE_STOP_CHILD", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if began.elapsed() > Duration::from_secs(3) {
                child.kill().unwrap();
                panic!("storage mutex blocked process stop");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = child.wait_with_output().unwrap();
        assert_eq!(status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("actual-sqlite-mutex-held-before-pid-cleanup")
        );
        assert!(began.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn signal_deadline_exits_storage_block_without_async_signal_polling() {
        use std::io::{BufRead, BufReader};
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "service::scoped_vault_shutdown::tests::actual_storage_lock_child",
                "--nocapture",
            ])
            .env("PERCIVAL_SYNTHETIC_STORAGE_STOP_CHILD", "signal")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if line.contains("actual-sqlite-mutex-held-before-pid-cleanup") {
                    let _ = tx.send(());
                    break;
                }
            }
        });
        if rx.recv_timeout(Duration::from_secs(3)).is_err() {
            child.kill().unwrap();
            panic!("storage fixture not ready");
        }
        let began = Instant::now();
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(1));
                break;
            }
            if began.elapsed() > Duration::from_secs(2) {
                child.kill().unwrap();
                panic!("SIGTERM stop depends on blocked async/storage execution");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
