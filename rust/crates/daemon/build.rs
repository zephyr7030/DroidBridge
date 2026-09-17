//! Links the bundled static libpcap into the Android daemon (S-NET-005).
//!
//! The library is an Android arm64 artifact produced by `tools/build-libpcap.ps1`, so the
//! directives are emitted only for the Android target; every other target compiles this crate
//! without a capture backend and without a stale search path.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    // rust/crates/daemon -> rust/crates -> rust -> the repository root.
    let root = std::path::Path::new(&manifest)
        .ancestors()
        .nth(3)
        .expect("the crate manifest lies inside the repository");
    let library = root.join("build/i0-cache/libpcap-1.10.6-android-arm64");
    println!("cargo:rustc-link-search=native={}", library.display());
    println!("cargo:rustc-link-lib=static=pcap");
}
