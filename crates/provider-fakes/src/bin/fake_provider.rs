//! The program a test puts where a command is expected, doing what the
//! plan beside it says (`provider_fakes::dispatch`).

fn main() {
    std::process::exit(provider_fakes::dispatch::main());
}
