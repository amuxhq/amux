//! `exe-resource <program> <version> <out>`: writes the `.res` file the
//! Windows program `<program>` (`amux`, `amux-launch`) at `<version>` is
//! linked with. Buck2's resource step (build/rust.bzl's
//! `windows_resource`) runs it; Cargo's build scripts call the library.

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [name, version, out] = arguments.as_slice() else {
        eprintln!("usage: exe-resource <program> <version> <out>");
        std::process::exit(2);
    };
    let Some(program) = exe_resource::program(name) else {
        eprintln!("exe-resource: amux ships no program named {name}");
        std::process::exit(2);
    };
    if let Err(error) = std::fs::write(out, exe_resource::resource_file(program, version)) {
        eprintln!("exe-resource: writing {out}: {error}");
        std::process::exit(1);
    }
}
