//! Windows gives the main thread a 1 MB stack by default; GPUI's element tree overflows it in
//! debug builds (the spike crashed with "main has overflowed its stack" before its first frame).
//! Zed links its binary with an 8 MB stack for the same reason (`crates/zed/build.rs`).

fn main() {
    let target = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") && target == "msvc" {
        println!("cargo:rustc-link-arg-bins=/STACK:{}", 8 * 1024 * 1024);
    }
}
