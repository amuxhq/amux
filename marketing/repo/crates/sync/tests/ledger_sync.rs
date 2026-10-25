#[tokio::test]
async fn two_devices_converge_after_offline_edits() {
    let (a, b) = devices().await;
    a.record(invoice(1, 4_200)).await;
    b.record(invoice(2, 1_500)).await;
    sync(&a, &b).await;
    assert_eq!(a.total().await, b.total().await);
}
