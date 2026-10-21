use models::{Bounds, SingleDaemon, check};

#[test]
#[ignore]
fn peer_nightly_figure() {
    let verdict = check(&SingleDaemon::peer_nightly(), Bounds::exhaustive().threads(models::threads()));
    println!("{verdict}");
    verdict.assert_holds();
}
