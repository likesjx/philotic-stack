use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=ui");
    emit_rerun_for_dir(Path::new("ui"));
}

fn emit_rerun_for_dir(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            println!("cargo:rerun-if-changed={}", path.display());
            if path.is_dir() {
                emit_rerun_for_dir(&path);
            }
        }
    }
}
