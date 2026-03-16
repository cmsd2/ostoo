fn main() {
    let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("x86_64-kernel.ld");
    println!("cargo:rustc-link-arg=-T{}", script.display());
    println!("cargo:rerun-if-changed={}", script.display());
}
