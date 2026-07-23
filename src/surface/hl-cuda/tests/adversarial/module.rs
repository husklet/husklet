use super::*;

// ===================================================================================================
// module + global — resolution invariants
// ===================================================================================================

#[test]
fn get_function_and_global_reject_unknown_module_ids() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // module id 999 was never loaded.
    assert!(load_module::module_get_function(&c, 999, "vecadd").is_err());
    // an unknown module yields Ok(None) for a global (NOT_FOUND at the ABI seam), never a fake pointer.
    assert_eq!(
        load_module::module_get_global(&mut c, &mut sink, 999, "g").unwrap(),
        None
    );
}

#[test]
fn same_global_name_in_two_modules_gets_distinct_backing_buffers() {
    const G: &str = ".visible .global .align 4 .b8 buf[128];\n.visible .entry k() { ret; }\n";
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let m1 = c.load_ptx(G);
    let m2 = c.load_ptx(G);
    let (p1, s1) = load_module::module_get_global(&mut c, &mut sink, m1, "buf")
        .unwrap()
        .unwrap();
    let (p2, s2) = load_module::module_get_global(&mut c, &mut sink, m2, "buf")
        .unwrap()
        .unwrap();
    assert_eq!((s1, s2), (128, 128));
    assert_ne!(
        p1, p2,
        "the same symbol in two modules must not alias one backing buffer"
    );
    assert!(c.mem.containing(p1).is_some() && c.mem.containing(p2).is_some());
}

#[test]
fn load_data_rejects_non_utf8_non_fatbin_image() {
    let mut c = ctx();
    // Bytes that are neither a fatbin container nor valid UTF-8 PTX text → typed error, never a load.
    let junk = [0xFFu8, 0xFE, 0x00, 0x80, 0x81];
    assert!(!fatbin::Image::new(&junk).is_fatbin());
    assert!(c.load_module(&junk).is_err());
}
