//! Build script for philotic-web.
//!
//! Finds the jaredlikes-desktop repo (sibling of the philotic-stack workspace
//! or overridden via PHILOTIC_DESKTOP_DIR), runs `npm run build`, then copies
//! the resulting `dist/` into `crates/philotic-web/ui-dist/` so that
//! `rust-embed` can bake it into the binary at compile time.
//!
//! If the desktop repo is not found, a minimal placeholder `index.html` is
//! written so the build still succeeds (useful on CI without the desktop repo).

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ui_dist_dir = manifest_dir.join("ui-dist");

    // Locate the desktop repo
    let desktop_dir = resolve_desktop_dir(&manifest_dir);

    if let Some(ref dir) = desktop_dir {
        // Rerun when JS sources or package.json change
        println!("cargo:rerun-if-changed={}", dir.join("src").display());
        println!(
            "cargo:rerun-if-changed={}",
            dir.join("package.json").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            dir.join("vite.config.js").display()
        );

        build_desktop(dir);
        copy_dist(&dir.join("dist"), &ui_dist_dir);
    } else if !ui_dist_dir.join("index.html").exists() {
        // No desktop repo and no cached ui-dist — write a placeholder so
        // rust-embed doesn't fail the compile.
        std::fs::create_dir_all(&ui_dist_dir).unwrap();
        std::fs::write(
            ui_dist_dir.join("index.html"),
            r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="UTF-8"><title>Aiua — UI not built</title></head>
<body style="font-family:system-ui;padding:40px;background:#1c1c1e;color:#fff">
  <h2>Management UI not embedded</h2>
  <p>To embed the desktop UI, set <code>PHILOTIC_DESKTOP_DIR</code> to the
  path of the <code>jaredlikes-desktop</code> repo and rebuild:</p>
  <pre>PHILOTIC_DESKTOP_DIR=~/code/jaredlikes-desktop cargo build -p philotic-web</pre>
  <p>Or place a pre-built dist in <code>crates/philotic-web/ui-dist/</code>.</p>
</body>
</html>"#,
        )
        .unwrap();
    }
    // else: ui-dist already populated from a previous build — reuse it.

    println!("cargo:rerun-if-env-changed=PHILOTIC_DESKTOP_DIR");
    println!("cargo:rerun-if-changed=ui-dist/index.html");
}

/// Resolve the jaredlikes-desktop directory.
/// Priority: PHILOTIC_DESKTOP_DIR env var > sibling of the workspace root.
fn resolve_desktop_dir(manifest_dir: &Path) -> Option<PathBuf> {
    // Env override
    if let Ok(dir) = std::env::var("PHILOTIC_DESKTOP_DIR") {
        let p = PathBuf::from(dir);
        if p.exists() {
            return Some(p);
        }
        eprintln!(
            "cargo:warning=PHILOTIC_DESKTOP_DIR set but path does not exist: {}",
            p.display()
        );
    }

    // Workspace root is manifest_dir/../../  (crates/philotic-web → workspace)
    // Desktop repo is expected as a sibling of the workspace root.
    let workspace_root = manifest_dir.parent()?.parent()?;
    let sibling = workspace_root.parent()?.join("jaredlikes-desktop");
    if sibling.exists() {
        return Some(sibling);
    }

    eprintln!(
        "cargo:warning=jaredlikes-desktop not found at {} — embedding placeholder UI. \
         Set PHILOTIC_DESKTOP_DIR to build with the real UI.",
        sibling.display()
    );
    None
}

fn build_desktop(dir: &Path) {
    println!("cargo:warning=Building jaredlikes-desktop UI...");

    // Install deps if node_modules is missing
    if !dir.join("node_modules").exists() {
        let status = Command::new("npm")
            .args(["install"])
            .current_dir(dir)
            .status()
            .expect("Failed to run npm install");
        assert!(status.success(), "npm install failed");
    }

    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(dir)
        .status()
        .expect("Failed to run npm run build");

    assert!(
        status.success(),
        "npm run build failed in {}",
        dir.display()
    );
}

fn copy_dist(src: &Path, dst: &Path) {
    if dst.exists() {
        std::fs::remove_dir_all(dst).unwrap();
    }
    copy_dir_all(src, dst).unwrap_or_else(|e| {
        panic!(
            "Failed to copy {} → {}: {}",
            src.display(),
            dst.display(),
            e
        )
    });
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}
