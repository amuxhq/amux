use ios_tools::{app_header, ios_verify, kotlin_types, markdown_fixtures, swift_types};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("verify") => ios_verify::run(),
        Some("app-header") => app_header::main(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("swift-types") => swift_types::main(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("kotlin-types") => kotlin_types::main(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("markdown-fixtures") => {
            markdown_fixtures::main(&std::env::args().skip(2).collect::<Vec<_>>())
        }
        _ => {
            eprintln!(
                "usage: ios-tools <app-header <app-ffi dir> <out>|verify [--gate|--captures|--shipping|--measured]|swift-types [--check]|kotlin-types [--check|--values PATH]|markdown-fixtures [--check]>"
            );
            std::process::exit(2);
        }
    }
}
