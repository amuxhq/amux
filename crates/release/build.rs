// Manifests are keyed by target triple; this is the one place it is known.
fn main() {
    println!(
        "cargo:rustc-env=AMUX_TARGET={}",
        std::env::var("TARGET").expect("cargo sets TARGET")
    );
    println!("cargo:rerun-if-changed=build.rs");
}
