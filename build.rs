//! Stamps the compile instant so the running binary can report when it was built.
//!
//! Emits one env var, unix seconds, consumed by `src/build_info.rs` as
//! `env!("GHOSTWAVE_BUILD_EPOCH")`. No `rerun-if-changed` is emitted on purpose:
//! cargo's default then reruns this whenever any file in the package changes, so
//! the stamp moves on every real recompile instead of freezing after the first.

fn main() {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=GHOSTWAVE_BUILD_EPOCH={secs}");
}
