//! `ios-tools app-header <app-ffi dir> <out>`: the app's C header, from
//! app-ffi's Rust sources and its cbindgen.toml.
//!
//! app-ffi's build script writes the same header into Cargo's `OUT_DIR`.
//! Buck can't run that script: app-ffi is a static library only, and the
//! tool that turns the workspace's crates into Buck targets reaches a
//! crate's build script through its library target, so cbindgen never
//! joins the third-party graph. Buck builds the header with this command
//! instead (`//crates/app-ffi:header`). Both parse the sources alone,
//! never asking Cargo for the workspace's metadata, so both produce the
//! same file.

use std::path::Path;

pub fn main(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let [crate_dir, out] = args else {
        return Err("usage: ios-tools app-header <app-ffi dir> <out>".into());
    };
    write(Path::new(crate_dir), Path::new(out))
}

pub fn write(crate_dir: &Path, out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = cbindgen::Config::from_file(crate_dir.join("cbindgen.toml"))?;
    cbindgen::Builder::new()
        .with_src(crate_dir.join("src/lib.rs"))
        .with_config(config)
        .generate()?
        .write_to_file(out);
    Ok(())
}
