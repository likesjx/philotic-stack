//! Public dummy fixture using the exact production startup module. Never opens the broker DB.
#[path = "../src/private_start.rs"] mod private_start;
use anyhow::{ensure, Result};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
fn main() -> Result<()> {
    private_start::enter()?;
    let home=std::env::var("HOME")?;
    let meta=std::fs::metadata(format!("{home}/.philotic/vault-master-key.env"))?;
    ensure!(meta.uid()==999 && meta.mode()&0o777==0o400,"file ownership");
    let mounts=std::fs::read_to_string("/proc/mounts")?;
    ensure!(mounts.lines().any(|line|{let v:Vec<_>=line.split_whitespace().collect();v.len()>3 && v[1]==home && v[3].split(',').any(|x|x=="noswap")}),"noswap mount");
    let value=std::fs::read(format!("{home}/.philotic/vault-master-key.env"))?;
    ensure!(value==b"PHILOTIC_VAULT_MASTER_KEY=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=\n", "public fixture key");
    let request:serde_json::Value=serde_json::from_reader(std::io::stdin())?;
    let timeout=request["timeout"].as_bool().unwrap_or(false);
    let _key_fd=std::fs::File::open(format!("{home}/.philotic/vault-master-key.env"))?;
    println!("{{\"fixture_ready\":true,\"pid\":{},\"uid\":999,\"key_fd\":{}}}",std::process::id(),_key_fd.as_raw_fd());
    std::thread::sleep(std::time::Duration::from_secs(if timeout {60} else {3}));
    Ok(())
}
