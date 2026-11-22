use ios_tools::{ios_verify, kotlin_types, markdown_fixtures, swift_types};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("verify") => ios_verify::run(),
        Some("swift-types") => swift_types::main(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("kotlin-types") => kotlin_types::main(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("markdown-fixtures") => {
            markdown_fixtures::main(&std::env::args().skip(2).collect::<Vec<_>>())
        }
        _ => {
            eprintln!(
                "usage: ios-tools <verify [--gate|--captures|--shipping|--measured]|swift-types [--check]|kotlin-types [--check]|markdown-fixtures [--check]>"
            );
            std::process::exit(2);
        }
    }
}
