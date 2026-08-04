pub struct Appearance;

impl Appearance {
    pub fn apply() {
        #[cfg(target_os = "macos")]
        Self::macos();
    }

    #[cfg(target_os = "macos")]
    fn macos() {
        use objc2_app_kit::{NSAppearance, NSApplication};
        use objc2_foundation::{MainThreadMarker, NSString};

        let Some(main) = MainThreadMarker::new() else {
            return;
        };
        let application = NSApplication::sharedApplication(main);
        let name = NSString::from_str("NSAppearanceNameDarkAqua");
        if let Some(appearance) = NSAppearance::appearanceNamed(&name) {
            application.setAppearance(Some(&appearance));
        }
    }
}
