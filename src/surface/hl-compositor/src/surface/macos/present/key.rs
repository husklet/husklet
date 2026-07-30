use std::ffi::c_void;

use objc2_foundation::NSString;

use super::{NSEventModifierFlags, PresenterEvent};

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardInputSource() -> *const c_void;
    fn TISGetInputSourceProperty(source: *const c_void, property: *const c_void) -> *const c_void;
    static kTISPropertyInputSourceID: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
}

/// Translate AppKit's active input-source identifier to the XKB layout used by Wayland clients.
///
/// Native events describe physical keys. The compositor must publish the matching logical XKB map or
/// punctuation and number-row symbols follow US positions instead of the user's selected macOS layout.
pub(super) fn xkb_layout(source: &str) -> Option<&'static str> {
    let name: String = source
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();

    [
        ("belgian", "be"),
        ("brazilian", "br"),
        ("british", "gb"),
        ("bulgarian", "bg"),
        ("canadian", "ca"),
        ("croatian", "hr"),
        ("czech", "cz"),
        ("danish", "dk"),
        ("dutch", "nl"),
        ("estonian", "ee"),
        ("finnish", "fi"),
        ("french", "fr"),
        ("german", "de"),
        ("greek", "gr"),
        ("hungarian", "hu"),
        ("icelandic", "is"),
        ("irish", "ie"),
        ("italian", "it"),
        ("latvian", "lv"),
        ("lithuanian", "lt"),
        ("norwegian", "no"),
        ("polish", "pl"),
        ("portuguese", "pt"),
        ("romanian", "ro"),
        ("russian", "ru"),
        ("slovak", "sk"),
        ("slovenian", "si"),
        ("spanish", "es"),
        ("swedish", "se"),
        ("swiss", "ch"),
        ("turkish", "tr"),
        ("ukrainian", "ua"),
        ("us", "us"),
        ("abc", "us"),
    ]
    .into_iter()
    .find_map(|(input, layout)| name.ends_with(input).then_some(layout))
}

pub(super) fn current_xkb_layout() -> Option<&'static str> {
    unsafe {
        let source = TISCopyCurrentKeyboardInputSource();
        if source.is_null() {
            return None;
        }
        let property = TISGetInputSourceProperty(source, kTISPropertyInputSourceID);
        let identifier = property
            .cast::<NSString>()
            .as_ref()
            .map(ToString::to_string);
        CFRelease(source);
        identifier.as_deref().and_then(xkb_layout)
    }
}

/// macOS virtual key code to Linux evdev. Covers ANSI, navigation, function, and numeric-keypad keys;
/// unknown media/vendor keys are ignored instead of emitting the wrong key.
pub(super) struct KeyCode(u16);

impl From<u16> for KeyCode {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

#[derive(Default)]
pub(super) struct Modifiers(u8);

impl Modifiers {
    const CTRL: u8 = 1;
    const SHIFT: u8 = 2;
    const ALT: u8 = 4;
    const CAPS: u8 = 8;

    pub(super) fn update(&mut self, flags: NSEventModifierFlags) -> Vec<PresenterEvent> {
        let mut next = 0;
        if flags.intersects(
            NSEventModifierFlags::NSEventModifierFlagCommand
                | NSEventModifierFlags::NSEventModifierFlagControl,
        ) {
            next |= Self::CTRL;
        }
        if flags.contains(NSEventModifierFlags::NSEventModifierFlagShift) {
            next |= Self::SHIFT;
        }
        if flags.contains(NSEventModifierFlags::NSEventModifierFlagOption) {
            next |= Self::ALT;
        }
        if flags.contains(NSEventModifierFlags::NSEventModifierFlagCapsLock) {
            next |= Self::CAPS;
        }

        let mut events = Vec::with_capacity(4);
        for (bit, keycode) in [(Self::CTRL, 29), (Self::SHIFT, 42), (Self::ALT, 56)] {
            if self.0 & bit != next & bit {
                events.push(PresenterEvent::Key {
                    keycode,
                    pressed: next & bit != 0,
                });
            }
        }
        // Caps Lock is a toggle. AppKit reports its latched flag changing rather than a conventional
        // key-down/key-up pair, while Linux/XKB toggles on each key press. Emit one complete tap for every
        // state transition so both turning it on and turning it off reach the guest correctly.
        if self.0 & Self::CAPS != next & Self::CAPS {
            events.extend([
                PresenterEvent::Key {
                    keycode: 58,
                    pressed: true,
                },
                PresenterEvent::Key {
                    keycode: 58,
                    pressed: false,
                },
            ]);
        }
        self.0 = next;
        events
    }
}

impl KeyCode {
    pub(super) fn event(self, pressed: bool, repeated: bool) -> Option<PresenterEvent> {
        if repeated {
            return None;
        }
        self.evdev()
            .map(|keycode| PresenterEvent::Key { keycode, pressed })
    }

    pub(super) fn evdev(self) -> Option<u32> {
        Some(match self.0 {
            0 => 30,
            1 => 31,
            2 => 32,
            3 => 33,
            4 => 35,
            5 => 34,
            6 => 44,
            7 => 45,
            8 => 46,
            9 => 47,
            11 => 48,
            12 => 16,
            13 => 17,
            14 => 18,
            15 => 19,
            16 => 21,
            17 => 20,
            18 => 2,
            19 => 3,
            20 => 4,
            21 => 5,
            22 => 7,
            23 => 6,
            24 => 13,
            25 => 10,
            26 => 8,
            27 => 12,
            28 => 9,
            29 => 11,
            30 => 27,
            31 => 24,
            32 => 22,
            33 => 26,
            34 => 23,
            35 => 25,
            36 => 28,
            37 => 38,
            38 => 36,
            39 => 40,
            40 => 37,
            41 => 39,
            42 => 43,
            43 => 51,
            44 => 53,
            45 => 49,
            46 => 50,
            47 => 52,
            48 => 15,
            49 => 57,
            50 => 41,
            51 => 14,
            53 => 1,
            64 => 187,
            65 => 83,
            67 => 55,
            69 => 78,
            71 => 69,
            75 => 98,
            76 => 96,
            78 => 74,
            79 => 188,
            80 => 189,
            81 => 117,
            82 => 82,
            83 => 79,
            84 => 80,
            85 => 81,
            86 => 75,
            87 => 76,
            88 => 77,
            89 => 71,
            91 => 72,
            92 => 73,
            96 => 63,
            97 => 64,
            98 => 65,
            99 => 61,
            100 => 66,
            101 => 67,
            103 => 87,
            105 => 183,
            106 => 186,
            107 => 184,
            109 => 68,
            111 => 88,
            113 => 185,
            114 => 110,
            115 => 102,
            116 => 104,
            117 => 111,
            118 => 62,
            119 => 107,
            120 => 60,
            121 => 109,
            122 => 59,
            123 => 105,
            124 => 106,
            125 => 108,
            126 => 103,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{xkb_layout, KeyCode, Modifiers, NSEventModifierFlags, PresenterEvent};

    #[test]
    fn maps_macos_input_sources_to_xkb_layouts() {
        assert_eq!(xkb_layout("com.apple.keylayout.Czech"), Some("cz"));
        assert_eq!(xkb_layout("com.apple.keylayout.British"), Some("gb"));
        assert_eq!(xkb_layout("com.apple.keylayout.ABC"), Some("us"));
        assert_eq!(xkb_layout("third.party.input-method"), None);
    }

    #[test]
    fn maps_every_standard_keyboard_region() {
        for (macos, evdev) in [
            (0, 30),
            (18, 2),
            (36, 28),
            (51, 14),
            (122, 59),
            (111, 88),
            (115, 102),
            (121, 109),
            (123, 105),
            (126, 103),
            (82, 82),
            (92, 73),
            (65, 83),
            (76, 96),
        ] {
            assert_eq!(
                KeyCode::from(macos).evdev(),
                Some(evdev),
                "macOS key {macos}"
            );
        }
    }

    #[test]
    fn unknown_vendor_keys_are_not_mistranslated() {
        assert_eq!(KeyCode::from(u16::MAX).evdev(), None);
    }

    #[test]
    fn appkit_repeat_is_left_to_the_wayland_repeat_contract() {
        assert!(KeyCode::from(0).event(true, true).is_none());
        assert!(matches!(
            KeyCode::from(0).event(true, false),
            Some(PresenterEvent::Key {
                keycode: 30,
                pressed: true
            })
        ));
    }

    #[test]
    fn caps_lock_emits_a_complete_toggle_tap_on_both_transitions() {
        let mut modifiers = Modifiers::default();
        let caps = NSEventModifierFlags::NSEventModifierFlagCapsLock;

        for events in [
            modifiers.update(caps),
            modifiers.update(NSEventModifierFlags::empty()),
        ] {
            assert!(matches!(
                events.as_slice(),
                [
                    PresenterEvent::Key {
                        keycode: 58,
                        pressed: true
                    },
                    PresenterEvent::Key {
                        keycode: 58,
                        pressed: false
                    }
                ]
            ));
        }
    }
}
