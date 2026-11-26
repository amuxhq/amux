// amux-launch.exe's application manifest and version information
// (crates/exe-resource); Buck2 links the same file (build/rust.bzl's
// `windows_resource`).
fn main() {
    exe_resource::build_script("amux-launch");
}
