//! The XKB keymap the compositor hands to `wl_keyboard` clients, plus an anonymous-fd helper.
//!
//! A Wayland client `mmap`s the `wl_keyboard.keymap` fd and feeds the text to `xkb_keymap_new_from_string`
//! to build its `xkb_state`. Toolkits (SDL2/GTK/Chrome) refuse to initialize keyboard input without a
//! valid keymap, so we ship a compact but complete US/evdev keymap. (The eventual mac backend can replace
//! this with an `xkb_keymap_get_as_string` result generated from the host layout for full fidelity.)

/// Anonymous shared fd holding `bytes` (+ trailing NUL), sized to fit. `memfd_create` on Linux,
/// `shm_open`+`shm_unlink` on macOS — the same portable anonymous-file trick the engine's
/// `memfd_create` uses. The caller passes it to a client via `SCM_RIGHTS`.
pub fn anon_fd_with(bytes: &[u8]) -> Option<i32> {
    let size = bytes.len() + 1; // + NUL
    #[cfg(target_os = "linux")]
    let fd = {
        let name = std::ffi::CString::new("dd-keymap").ok()?;
        unsafe { libc::memfd_create(name.as_ptr(), 0) }
    };
    #[cfg(not(target_os = "linux"))]
    let fd = {
        // A per-process monotonic counter guarantees a unique shm name across CONCURRENT callers.
        // `subsec_nanos()` alone collides when parallel test threads (same pid) hit the same clock tick,
        // and O_EXCL then fails EEXIST — the source of a flaky `anon_fd_with` failure on macOS.
        use std::sync::atomic::{AtomicU64, Ordering};
        static ANON_SEQ: AtomicU64 = AtomicU64::new(0);
        let nm = format!("/ddkm{}.{}", unsafe { libc::getpid() }, ANON_SEQ.fetch_add(1, Ordering::Relaxed));
        let c = std::ffi::CString::new(nm).ok()?;
        unsafe {
            let fd = libc::shm_open(
                c.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o600,
            );
            if fd >= 0 {
                libc::shm_unlink(c.as_ptr());
            }
            fd
        }
    };
    if fd < 0 {
        return None;
    }
    unsafe {
        if libc::ftruncate(fd, size as libc::off_t) != 0 {
            libc::close(fd);
            return None;
        }
        let map = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        );
        if map == libc::MAP_FAILED {
            libc::close(fd);
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), map as *mut u8, bytes.len());
        *(map as *mut u8).add(bytes.len()) = 0; // NUL
        libc::munmap(map, size);
    }
    Some(fd)
}

/// A compact US keymap in the `XKB_KEYMAP_FORMAT_TEXT_V1` format. Evdev keycodes (name + 8), standard
/// types, and us symbols for the alphanumerics + common keys — enough for a toolkit to build a working
/// `xkb_state` and translate the raw evdev codes we send in `wl_keyboard.key`.
pub const US_XKB_KEYMAP: &str = r#"xkb_keymap {
xkb_keycodes "dd" {
    minimum = 8;
    maximum = 255;
    <ESC> = 9;
    <AE01> = 10; <AE02> = 11; <AE03> = 12; <AE04> = 13; <AE05> = 14;
    <AE06> = 15; <AE07> = 16; <AE08> = 17; <AE09> = 18; <AE10> = 19;
    <AE11> = 20; <AE12> = 21;
    <AD01> = 24; <AD02> = 25; <AD03> = 26; <AD04> = 27; <AD05> = 28;
    <AD06> = 29; <AD07> = 30; <AD08> = 31; <AD09> = 32; <AD10> = 33;
    <AD11> = 34; <AD12> = 35;
    <AC01> = 38; <AC02> = 39; <AC03> = 40; <AC04> = 41; <AC05> = 42;
    <AC06> = 43; <AC07> = 44; <AC08> = 45; <AC09> = 46;
    <AC10> = 47; <AC11> = 48;
    <AB01> = 52; <AB02> = 53; <AB03> = 54; <AB04> = 55; <AB05> = 56;
    <AB06> = 57; <AB07> = 58; <AB08> = 59; <AB09> = 60; <AB10> = 61;
    <TLDE> = 49; <BKSL> = 51;
    <SPCE> = 65; <RTRN> = 36; <TAB> = 23; <BKSP> = 22;
    <LFSH> = 50; <RTSH> = 62; <LCTL> = 37; <RCTL> = 105; <LALT> = 64; <RALT> = 108;
    <LWIN> = 133; <CAPS> = 66;
    <LEFT> = 113; <RGHT> = 114; <UP> = 111; <DOWN> = 116;
};
xkb_types "dd" {
    virtual_modifiers NumLock,Alt,LevelThree,Super,Meta;
    type "ONE_LEVEL" { modifiers = none; level_name[Level1] = "Any"; };
    type "TWO_LEVEL" {
        modifiers = Shift;
        map[Shift] = Level2;
        level_name[Level1] = "Base"; level_name[Level2] = "Shift";
    };
    type "ALPHABETIC" {
        modifiers = Shift+Lock;
        map[Shift] = Level2; map[Lock] = Level2;
        level_name[Level1] = "Base"; level_name[Level2] = "Caps";
    };
};
xkb_compatibility "dd" {
    virtual_modifiers NumLock,Alt,LevelThree,Super,Meta;
    // Keys repeat by default — consistent with the wl_keyboard.repeat_info(rate>0) the compositor
    // advertises. (Leaving this False would tell xkbcommon "no key repeats" while the protocol said
    // repeat is enabled, so toolkits combining the two would behave inconsistently.)
    interpret.repeat = True;
    interpret Any + AnyOf(all) { action = SetMods(modifiers = modMapMods); };
    interpret Caps_Lock { action = LockMods(modifiers = Lock); };
    interpret Num_Lock { action = LockMods(modifiers = NumLock); };
};
xkb_symbols "dd" {
    name[Group1] = "English (US)";
    key <ESC> { [ Escape ] };
    key <AE01> { [ 1, exclam ] }; key <AE02> { [ 2, at ] }; key <AE03> { [ 3, numbersign ] };
    key <AE04> { [ 4, dollar ] }; key <AE05> { [ 5, percent ] }; key <AE06> { [ 6, asciicircum ] };
    key <AE07> { [ 7, ampersand ] }; key <AE08> { [ 8, asterisk ] }; key <AE09> { [ 9, parenleft ] };
    key <AE10> { [ 0, parenright ] };
    key <AE11> { [ minus, underscore ] }; key <AE12> { [ equal, plus ] };
    key <AD01> { [ q, Q ] }; key <AD02> { [ w, W ] }; key <AD03> { [ e, E ] }; key <AD04> { [ r, R ] };
    key <AD05> { [ t, T ] }; key <AD06> { [ y, Y ] }; key <AD07> { [ u, U ] }; key <AD08> { [ i, I ] };
    key <AD09> { [ o, O ] }; key <AD10> { [ p, P ] };
    key <AD11> { [ bracketleft, braceleft ] }; key <AD12> { [ bracketright, braceright ] };
    key <AC01> { [ a, A ] }; key <AC02> { [ s, S ] }; key <AC03> { [ d, D ] }; key <AC04> { [ f, F ] };
    key <AC05> { [ g, G ] }; key <AC06> { [ h, H ] }; key <AC07> { [ j, J ] }; key <AC08> { [ k, K ] };
    key <AC09> { [ l, L ] };
    key <AC10> { [ semicolon, colon ] }; key <AC11> { [ apostrophe, quotedbl ] };
    key <AB01> { [ z, Z ] }; key <AB02> { [ x, X ] }; key <AB03> { [ c, C ] }; key <AB04> { [ v, V ] };
    key <AB05> { [ b, B ] }; key <AB06> { [ n, N ] }; key <AB07> { [ m, M ] };
    key <AB08> { [ comma, less ] }; key <AB09> { [ period, greater ] }; key <AB10> { [ slash, question ] };
    key <TLDE> { [ grave, asciitilde ] }; key <BKSL> { [ backslash, bar ] };
    key <SPCE> { [ space ] }; key <RTRN> { [ Return ] }; key <TAB> { [ Tab ] }; key <BKSP> { [ BackSpace ] };
    key <LEFT> { [ Left ] }; key <RGHT> { [ Right ] }; key <UP> { [ Up ] }; key <DOWN> { [ Down ] };
    key <LFSH> { [ Shift_L ] }; key <RTSH> { [ Shift_R ] };
    key <LCTL> { [ Control_L ] }; key <RCTL> { [ Control_R ] };
    key <LALT> { [ Alt_L ] }; key <RALT> { [ Alt_R ] };
    key <LWIN> { [ Super_L ] }; key <CAPS> { [ Caps_Lock ] };
    modifier_map Shift { <LFSH>, <RTSH> };
    modifier_map Control { <LCTL>, <RCTL> };
    modifier_map Mod1 { <LALT> };
    modifier_map Mod4 { <LWIN> };
    modifier_map Lock { <CAPS> };
};
};
"#;
