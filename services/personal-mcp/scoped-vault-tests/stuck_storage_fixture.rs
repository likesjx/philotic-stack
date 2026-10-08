//! Subprocess-only demonstration of Tokio's uncooperative blocking-job shutdown.
//! Synthetic jobs; no hotel, storage, credentials or production supervisor.
use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{runtime::Builder, sync::Semaphore};
fn main() {
    let mode = std::env::args().nth(1).expect("fixture mode required");
    assert!(matches!(mode.as_str(), "drop" | "bounded"));
    let runtime = Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let slots = Arc::new(Semaphore::new(4));
    let entered = Arc::new(AtomicUsize::new(0));
    for _ in 0..4 {
        let permit = slots.clone().try_acquire_owned().unwrap();
        let entered = entered.clone();
        runtime.spawn_blocking(move || {
            let _held = permit;
            entered.fetch_add(1, Ordering::SeqCst);
            loop {
                std::thread::park();
            }
        });
    }
    let started = Instant::now();
    while entered.load(Ordering::SeqCst) != 4 {
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::yield_now();
    }
    assert_eq!(slots.available_permits(), 0);
    assert!(slots.clone().try_acquire_owned().is_err());
    println!("four-synthetic-blocking-jobs-started");
    io::stdout().flush().unwrap();
    if mode == "drop" {
        drop(runtime);
    } else {
        runtime.shutdown_timeout(Duration::from_millis(50));
    }
    println!("runtime-shutdown-returned");
}
