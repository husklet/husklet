use crate::{GuestPathBytes, OpenDecision, OpenDirectory, OpenIntent, OpenPlan, OpenRequest, OverlayAction};

#[test]
fn open_plan_identity() {
    let path = GuestPathBytes::new(b"/tmp/\xff").unwrap();
    let plan = OpenPlan::build(OpenRequest {
        guest_path: path.clone(),
        directory: OpenDirectory::from_raw(7),
        intent: OpenIntent::from_bits(OpenIntent::WRITE),
        overlay: true,
        read_only: false,
        final_symlink: false,
    })
    .unwrap();

    assert_eq!(plan.path(), &path);
    assert_eq!(plan.decision(), OpenDecision::Overlay(OverlayAction::CopyUp),);
}
