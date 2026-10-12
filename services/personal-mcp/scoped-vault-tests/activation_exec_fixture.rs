//! Synthetic subprocess probe of actual optional activation and exec inheritance.
#[cfg(target_os = "linux")]
fn main() {
    use percival_scoped_vault_tests::scoped_vault_activation;
    assert_eq!(
        std::env::var("PERCIVAL_ISOLATED_LINUX_FIXTURE").as_deref(),
        Ok("1")
    );
    assert!(std::path::Path::new("/.dockerenv").exists());
    // Single-threaded fixture only, before creating runtime threads. Real hotel
    // never rewrites its supervisor PID: this emulates the test supervisor.
    if std::env::var("PERCIVAL_FIXTURE_SELF_PID").as_deref() == Ok("1") {
        unsafe {
            std::env::set_var("LISTEN_PID", std::process::id().to_string());
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let listener = match scoped_vault_activation::optional() {
        Ok(listener) => listener,
        Err(_) => {
            println!("activation-rejected-before-guest-exec");
            std::process::exit(3);
        }
    };
    if std::env::args().nth(1).as_deref() == Some("probe") {
        assert!(listener.is_none());
        println!("exec-child-has-no-scoped-descriptor");
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("probe")
        .env("PHILOTIC_SCOPED_VAULT_ENABLED", "0")
        .env_remove("LISTEN_PID")
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_FDNAMES")
        .env_remove("PERCIVAL_FIXTURE_SELF_PID")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "scoped descriptor inherited by exec child"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("exec-child-has-no-scoped-descriptor")
    );
    println!("guest-exec-checked-no-scoped-descriptor");
    drop(listener);
}
#[cfg(not(target_os = "linux"))]
fn main() {
    std::process::exit(2);
}
