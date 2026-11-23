use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let config = cbindgen::Config::from_file(crate_dir.join("cbindgen.toml"))
        .expect("read app C header configuration");
    // The sources alone, as `ios-tools app-header` (which Buck runs)
    // reads them: with_crate would ask Cargo for the workspace's metadata.
    cbindgen::Builder::new()
        .with_src(crate_dir.join("src/lib.rs"))
        .with_config(config)
        .generate()
        .expect("generate app C header")
        .write_to_file(out_dir.join("amux_app.h"));
}
