use super::*;
use objc2::runtime::ProtocolObject;
use objc2::{declare_class, msg_send_id, mutability, ClassType, DeclaredClass};
use objc2_app_kit::{NSApplicationDelegate, NSApplicationTerminateReply};
use objc2_foundation::{NSObject, NSObjectProtocol};

declare_class!(
    struct CompositorApplicationDelegate;

    // SAFETY: NSObject has no subclass invariants, the delegate is created and used only on
    // AppKit's main thread, and the class has no custom destruction requirements.
    unsafe impl ClassType for CompositorApplicationDelegate {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "HLCompositorApplicationDelegate";
    }

    impl DeclaredClass for CompositorApplicationDelegate {
        type Ivars = ();
    }

    unsafe impl NSObjectProtocol for CompositorApplicationDelegate {}

    unsafe impl NSApplicationDelegate for CompositorApplicationDelegate {
        #[method(applicationShouldTerminate:)]
        unsafe fn should_terminate(
            &self,
            _application: &NSApplication,
        ) -> NSApplicationTerminateReply {
            hl_log::hl_debug!(
                hl_log::tag::COMPOSITOR,
                "ignoring AppKit termination request; compositor lifetime is owned by its control pipe"
            );
            NSApplicationTerminateReply::NSTerminateCancel
        }
    }
);

impl CompositorApplicationDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let object = mtm.alloc().set_ivars(());
        // SAFETY: `object` is an allocated NSObject subclass with all declared ivars initialized.
        unsafe { msg_send_id![super(object), init] }
    }
}

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

    pub fn primary_scale(&self) -> f64 {
        NSScreen::mainScreen(self.marker)
            .map(|screen| screen.backingScaleFactor())
            .unwrap_or(1.0)
            .max(1.0)
    }
}

/// Process-level AppKit bootstrap shared by all native windows.
pub struct NativeApplication {
    _application: Retained<NSApplication>,
    _delegate: Retained<CompositorApplicationDelegate>,
}

impl NativeApplication {
    pub fn ensure(mtm: MainThreadMarker) -> Self {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        let delegate = CompositorApplicationDelegate::new(mtm);
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        unsafe { app.finishLaunching() };
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        Self {
            _application: app,
            _delegate: delegate,
        }
    }
}
