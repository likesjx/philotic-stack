use std::{env, fs, path::PathBuf};
fn main() {
    let source = PathBuf::from("../../../crates/aiua/src/vault.rs");
    println!("cargo:rerun-if-changed={}", source.display());
    let text = fs::read_to_string(source).unwrap();
    let resolve = &text[text.find("pub fn resolve_secret(").unwrap()
        ..text.find("/// Read and decrypt a vault secret").unwrap()];
    let crypto = &text[text.find("fn encrypt(").unwrap()..text.find("fn cipher()").unwrap()];
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("vault_functions.rs"),
        format!("{resolve}\n{crypto}"),
    )
    .unwrap();
}
