use super::product::{ProductBackend, product_arm_error, product_order};

#[test]
fn product_pair_order_balances_first_position() {
    assert_eq!(product_order(0), [ProductBackend::ExplicitC, ProductBackend::DefaultC]);
    assert_eq!(product_order(1), [ProductBackend::DefaultC, ProductBackend::ExplicitC]);
    assert_eq!(product_order(2), [ProductBackend::ExplicitC, ProductBackend::DefaultC]);
    assert_eq!(product_order(3), [ProductBackend::DefaultC, ProductBackend::ExplicitC]);
}

#[test]
fn product_arm_failure_names_context_and_preserves_source() {
    let error = product_arm_error(
        3,
        1,
        ProductBackend::DefaultC,
        "execution",
        std::io::Error::other("Construction(Start)"),
    );
    assert_eq!(
        error.to_string(),
        "product A/B round 3 position 1 backend default-c execution: Construction(Start)"
    );
    assert_eq!(error.source().unwrap().to_string(), "Construction(Start)");
}
