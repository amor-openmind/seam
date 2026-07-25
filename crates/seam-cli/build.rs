//! Records the target triple so `seam doctor` can report exactly which build is running.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=SEAM_BUILD_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
