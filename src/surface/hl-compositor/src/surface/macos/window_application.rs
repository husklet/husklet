use super::*;

/// Primary Mac display configuration in the compositor output format.
pub struct DisplayConfig {
    marker: MainThreadMarker,
}

impl DisplayConfig {
    pub fn new(marker: MainThreadMarker) -> Self {
        Self { marker }
    }

    pub fn primary_spec(&self) -> Option<String> {
        let screen = NSScreen::mainScreen(self.marker)?;
        let scale = screen.backingScaleFactor().max(1.0).round() as i32;
        let frame = screen.frame();
        let width = (frame.size.width * f64::from(scale)).round().max(1.0) as u32;
        let height = (frame.size.height * f64::from(scale)).round().max(1.0) as u32;
        Some(format!("{width}x{height}@0,0*{scale}"))
    }

    pub fn primary_refresh_millihz(&self) -> Option<i64> {
        let hz = unsafe { NSScreen::mainScreen(self.marker)?.maximumFramesPerSecond() };
        (hz > 0).then_some(hz as i64 * 1_000)
    }
}

/// Process-level AppKit bootstrap shared by all native windows.
pub struct NativeApplication;

impl NativeApplication {
    pub fn ensure(mtm: MainThreadMarker) -> Retained<NSApplication> {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        unsafe { app.finishLaunching() };
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        app
    }
}
