//! Where a crate's files in the checkout are when its tests and developer
//! tools read them at run time: fixtures, goldens, recordings, scripts.
//!
//! Cargo compiles a crate with `CARGO_MANIFEST_DIR` set to the crate's
//! absolute path in the checkout, so `env!("CARGO_MANIFEST_DIR")` names the
//! crate's files wherever its binaries later run. Buck compiles each crate
//! in a sandbox of its own and runs its tests in another, possibly on
//! another machine, so a path fixed at compile time names nothing at run
//! time. Under Buck the crate's path within the checkout is compiled in
//! instead (`AMUX_CRATE_DIR`, set by `buck/rust.bzl`), and resolved against
//! the checkout the binary runs in: `AMUX_SOURCE_ROOT` when it is set, else
//! the working directory, which Buck makes the checkout's root for a test
//! and which `just` makes it for a tool.
//!
//! `crate_dir!()` replaces `Path::new(env!("CARGO_MANIFEST_DIR"))`
//! wherever the path is used at run time. A path used at compile time
//! (`include_str!`) keeps `env!` or a relative path: Buck lays a crate's
//! sources out as they sit in the checkout.
//!
//! `bin!("amux")` replaces `env!("CARGO_BIN_EXE_amux")`, the binary of
//! the crate's own that an integration test runs, for the same reason:
//! Buck sets `CARGO_BIN_EXE_<name>` when the test runs, not when it
//! compiles, so a test compiles without waiting for the binary.

use std::path::PathBuf;

/// The calling crate's directory in the checkout: `crates/<name>`.
#[macro_export]
macro_rules! crate_dir {
    () => {
        $crate::resolve(env!("CARGO_MANIFEST_DIR"), option_env!("AMUX_CRATE_DIR"))
    };
}

/// The path of the crate's binary `name`, for an integration test to run:
/// a `&'static str`, as `env!` gives.
#[macro_export]
macro_rules! bin {
    ($name:literal) => {{
        static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        PATH.get_or_init(|| $crate::binary($name, option_env!(concat!("CARGO_BIN_EXE_", $name))))
            .as_str()
    }};
}

#[doc(hidden)]
pub fn binary(name: &str, compiled: Option<&str>) -> String {
    let path = std::env::var(format!("CARGO_BIN_EXE_{name}"))
        .ok()
        .or_else(|| compiled.map(str::to_owned))
        .unwrap_or_else(|| panic!("no CARGO_BIN_EXE_{name}: is {name} a binary of this crate?"));
    // Absolute, so it can be run from any working directory: Buck names it
    // relative to the checkout.
    std::path::absolute(&path)
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .unwrap_or(path)
}

#[doc(hidden)]
pub fn resolve(cargo_manifest_dir: &str, crate_dir: Option<&str>) -> PathBuf {
    match crate_dir {
        None => PathBuf::from(cargo_manifest_dir),
        Some(relative) => {
            let root = std::env::var_os("AMUX_SOURCE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            root.join(relative)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::resolve;

    #[test]
    fn cargo_builds_keep_cargos_path() {
        assert_eq!(
            resolve("/checkout/crates/wire", None),
            PathBuf::from("/checkout/crates/wire")
        );
    }

    #[test]
    fn buck_builds_resolve_against_the_checkout_they_run_in() {
        let resolved = resolve("/sandbox/__srcs", Some("crates/wire"));
        assert!(resolved.ends_with("crates/wire"), "{resolved:?}");
        assert!(!resolved.starts_with("/sandbox"), "{resolved:?}");
    }
}
