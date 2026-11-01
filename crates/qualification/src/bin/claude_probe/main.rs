#[cfg(unix)]
mod capture;
mod probe;
mod scrub;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    qualification::recording::live::stand_in_for("claude", "CLAUDE_CAPTURE_PROXY")?;
    probe::main()
}
