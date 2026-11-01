#[cfg(unix)]
mod capture;
mod probe;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    qualification::recording::live::stand_in_for("codex", "CODEX_CAPTURE_PROXY")?;
    probe::main()
}
