//! Keep-awake where amux holds no sleep assertion (Linux and the rest).

pub enum Want {
    OnPower,
    Always,
}

pub struct Held;

pub fn hold(_want: Want) -> Held {
    tracing::info!("this platform holds no sleep assertion");
    Held
}
