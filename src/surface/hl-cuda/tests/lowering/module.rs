use super::*;

// ---------------------------------------------------------------------------------------------------
// load_module
// ---------------------------------------------------------------------------------------------------

#[test]
fn module_load_and_get_function() {
    let mut c = ctx();
    let m = c.load_ptx(ptx::VECADD_PTX);
    let f = load_module::module_get_function(&c, m, "vecadd").unwrap();
    assert_eq!(f.module, m);
    assert_eq!(f.entry, 0);
    // an unknown entry is a typed error.
    assert!(load_module::module_get_function(&c, m, "nope").is_err());
}

/// Build a minimal single-entry uncompressed-PTX fatbin container around `ptx` bytes, matching the
/// layout the walker parses (16-byte container header + 64-byte entry header + payload).
fn build_fatbin(ptx: &[u8]) -> Vec<u8> {
    const FATBIN_MAGIC: u32 = 0xba55_ed50;
    let mut entry = vec![0u8; 64];
    entry[0..2].copy_from_slice(&1u16.to_le_bytes()); // kind = PTX
    entry[4..8].copy_from_slice(&64u32.to_le_bytes()); // entry header_size
    entry[8..16].copy_from_slice(&(ptx.len() as u64).to_le_bytes()); // payload_size
                                                                     // flags @40 = 0 (uncompressed)
    let fat_size = (entry.len() + ptx.len()) as u64;

    let mut out = vec![0u8; 16];
    out[0..4].copy_from_slice(&FATBIN_MAGIC.to_le_bytes());
    out[6..8].copy_from_slice(&16u16.to_le_bytes()); // header_size
    out[8..16].copy_from_slice(&fat_size.to_le_bytes());
    out.extend_from_slice(&entry);
    out.extend_from_slice(ptx);
    out
}

#[test]
fn module_load_data_walks_fatbin() {
    let mut c = ctx();
    let image = build_fatbin(ptx::VECADD_PTX.as_bytes());
    assert!(fatbin::Image::new(&image).is_fatbin());
    assert_eq!(
        fatbin::Image::new(&image).ptx().unwrap(),
        ptx::VECADD_PTX.as_bytes()
    );

    let m = c.load_module(&image).unwrap();
    assert!(load_module::module_get_function(&c, m, "vecadd").is_ok());
}

#[test]
fn module_load_data_accepts_raw_ptx_text() {
    let mut c = ctx();
    let m = c.load_module(ptx::VECADD_PTX.as_bytes()).unwrap();
    assert!(load_module::module_get_function(&c, m, "vecadd").is_ok());
}

/// A PTX module with two `.global`/`.const` variable declarations plus a kernel entry, for the
/// `cuModuleGetGlobal` lowering test.
const GLOBALS_PTX: &str = r#"
.version 7.0
.target sm_86
.address_size 64

.visible .global .align 4 .b8 gCounters[256];
.const .align 4 .f32 kCoeff[4];

.visible .entry noop() { ret; }
"#;

#[test]
fn ptx_parse_recovers_global_declarations() {
    use hl_cuda::model::module::{GlobalVar, PtxModule};
    let m = PtxModule::parse(GLOBALS_PTX);
    assert_eq!(m.entries, vec!["noop".to_string()]);
    // `.b8 gCounters[256]` → 256 bytes; `.f32 kCoeff[4]` → 4-byte elem × 4 = 16 bytes.
    assert_eq!(
        m.globals,
        vec![
            GlobalVar {
                name: "gCounters".into(),
                size: 256
            },
            GlobalVar {
                name: "kCoeff".into(),
                size: 16
            },
        ]
    );
    // A `ld.global`/`st.global` instruction inside a kernel body is NOT mistaken for a declaration.
    assert!(!PtxModule::parse(ptx::VECADD_PTX)
        .globals
        .iter()
        .any(|_| true));
}

#[test]
fn module_get_global_returns_backing_buffer_and_size() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let m = c.load_ptx(GLOBALS_PTX);

    // The global resolves to a live device pointer + its declared byte size, backed by exactly one
    // CreateBuffer sized to the global.
    let (ptr, size) = load_module::module_get_global(&mut c, &mut sink, m, "gCounters")
        .unwrap()
        .unwrap();
    assert_eq!(size, 256);
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::CreateBuffer(id, desc)] => {
            assert_eq!(*id, 1);
            assert_eq!(desc.size, 256);
        }
        other => panic!("expected one CreateBuffer, got {other:?}"),
    }
    // the returned pointer is a live device allocation of exactly the global's size.
    assert_eq!(c.mem.containing(ptr), Some((ptr.0, 256)));

    // repeat lookup returns the SAME pointer and emits no new buffer.
    let batches = sink.batches.len();
    let (ptr2, size2) = load_module::module_get_global(&mut c, &mut sink, m, "gCounters")
        .unwrap()
        .unwrap();
    assert_eq!((ptr2, size2), (ptr, 256));
    assert_eq!(
        sink.batches.len(),
        batches,
        "cached global emits no new CreateBuffer"
    );

    // a second, distinct global gets its own backing buffer.
    let (cptr, csize) = load_module::module_get_global(&mut c, &mut sink, m, "kCoeff")
        .unwrap()
        .unwrap();
    assert_eq!(csize, 16);
    assert_ne!(cptr, ptr);

    // an undeclared symbol is Ok(None) → NOT_FOUND at the ABI seam (no fake pointer, no submit).
    let batches = sink.batches.len();
    assert_eq!(
        load_module::module_get_global(&mut c, &mut sink, m, "missing").unwrap(),
        None
    );
    assert_eq!(sink.batches.len(), batches);
}

#[test]
fn fatbin_rejects_non_container() {
    assert!(!fatbin::Image::new(b"not a fatbin").is_fatbin());
    assert!(fatbin::Image::new(b"not a fatbin").ptx().is_none());
}
