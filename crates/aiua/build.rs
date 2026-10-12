mod build_version;

fn main() {
    println!("cargo:rerun-if-changed=build_version.rs");
    for name in ["PHILOTIC_RELEASE_TAG", "PHILOTIC_BUILD_SHA"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let read = |name| {
        std::env::var_os(name)
            .map(|value| value.into_string().expect("build metadata must be UTF-8"))
    };
    let tag = read("PHILOTIC_RELEASE_TAG");
    let sha = read("PHILOTIC_BUILD_SHA");
    let metadata = build_version::resolve(
        &std::env::var("CARGO_PKG_VERSION").unwrap(),
        tag.as_deref(),
        sha.as_deref(),
    )
    .expect("invalid aiua build metadata");
    println!(
        "cargo:rustc-env=PHILOTIC_VERSION_DISPLAY={} ({})",
        metadata.version, metadata.sha
    );
}
