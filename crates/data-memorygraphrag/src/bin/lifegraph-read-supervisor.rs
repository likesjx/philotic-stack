#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() {
    use data_memorygraphrag::read_supervisor::{load_config, run};
    let result = async {
        let arguments: Vec<_> = std::env::args_os().collect();
        if arguments.len() != 3 || arguments[1] != "--config" {
            anyhow::bail!("usage: lifegraph-read-supervisor --config ABSOLUTE_PRIVATE_MANIFEST");
        }
        let config = load_config(std::path::Path::new(&arguments[2]))?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        run(config, async move {
            tokio::select! { _=terminate.recv()=>{}, _=tokio::signal::ctrl_c()=>{} }
        })
        .await
    }
    .await;
    if result.is_err() {
        // Avoid leaking credentials or manifest values from nested errors.
        eprintln!(
            "LifeGraph supervisor unavailable; inspect approved configuration or reconcile uncertainty"
        );
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("LifeGraph read supervisor requires Linux kernel peer credentials");
    std::process::exit(1);
}
