use models::{Bounds, ClientModel, check};

#[test]
fn probe() {
    let env = |name: &str, default: u8| {
        std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    };
    let model = ClientModel {
        writes: env("WRITES", 2),
        lags: env("LAGS", 1),
        resets: env("RESETS", 1),
        pages: env("PAGES", 1),
        checkpoints: env("CHECKPOINTS", 1),
        ..ClientModel::ci()
    };
    let verdict = check(&model, Bounds::exhaustive().threads(models::threads()));
    println!("{verdict}");
    let m = verdict.measured;
    println!("states={} seconds={:.1}", m.states, m.seconds);
}
