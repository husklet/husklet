//! GOLDEN WIRE VECTORS — a byte-exact freeze of the hl-gpu IR wire format.
//!
//! Each case builds a representative `Cmd` stream, encodes it with the crate's real
//! [`encode_stream`], and asserts the bytes equal a checked-in hex snapshot. Any future change to the
//! wire encoding (a reordered field, a new tag, a changed integer width, a different string framing)
//! breaks these tests **on purpose** — that is the whole point of a golden vector ahead of a refactor
//! that must be proven behavior-preserving.
//!
//! Every case additionally asserts:
//!   * `decode_stream(encode_stream(x)) == x` — the codec round-trips losslessly, and
//!   * the frame-level `Cmd::frame` / `Cmd::decode_frame` round-trip for the first command,
//! and the suite pins the current [`WIRE_VERSION`] value explicitly so a bump is a conscious edit.
//!
//! Regenerate the hex (only when a wire change is intentional) by re-running the encoder over
//! [`golden_cases`] and pasting the new lowercase-hex strings below.

use hl_gpu::ir::*;
use hl_gpu::wire::Decoder;

// -------------------------------------------------------------------------------------------------
// case corpus — the streams whose bytes are frozen below
// -------------------------------------------------------------------------------------------------

fn golden_cases() -> Vec<(&'static str, Vec<Cmd>)> {
    use hl_gpu::ir::{buffer_usage as bu, texture_usage as tu};

    let create_buffer = vec![Cmd::CreateBuffer(
        1,
        BufferDesc { size: 256, usage: bu::STORAGE | bu::COPY_SRC | bu::COPY_DST, label: String::new() },
    )];

    let write_buffer = vec![Cmd::WriteBuffer { id: 1, offset: 0, data: vec![0xDE, 0xAD, 0xBE, 0xEF] }];

    let create_texture = vec![Cmd::CreateTexture(
        2,
        TextureDesc {
            width: 4,
            height: 4,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: tu::RENDER_TARGET | tu::COPY_SRC,
            label: String::new(),
        },
    )];

    // A command buffer with an encoder doing a clear (BeginRenderPass Clear + EndRenderPass) and a
    // buffer->buffer copy, signalling a fence — i.e. the Submit case.
    let command_buffer = vec![Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 2,
                    load: LoadOp::Clear,
                    clear: [1.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::EndRenderPass,
            Enc::CopyBufferToBuffer { src: 1, src_offset: 0, dst: 3, dst_offset: 0, size: 16 },
        ],
        signal: Some((7, 42)),
    })];

    let present = vec![Cmd::Present { surface: 1, texture: 2 }];

    let destroy = vec![Cmd::DestroyBuffer(1), Cmd::DestroyTexture(2)];

    // The full representative stream, in the order a real frame would issue it: create resources,
    // upload, record + submit a clear-and-copy command buffer, present, then tear down.
    let full_stream = vec![
        Cmd::CreateBuffer(1, BufferDesc { size: 256, usage: bu::STORAGE | bu::COPY_SRC | bu::COPY_DST, label: String::new() }),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vec![0xDE, 0xAD, 0xBE, 0xEF] },
        Cmd::CreateBuffer(3, BufferDesc { size: 256, usage: bu::COPY_DST, label: String::new() }),
        Cmd::CreateTexture(2, TextureDesc {
            width: 4, height: 4, depth: 1, mip_levels: 1, sample_count: 1,
            dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm,
            usage: tu::RENDER_TARGET | tu::COPY_SRC | tu::PRESENT, label: String::new(),
        }),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 2, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
                    depth: None,
                },
                Enc::EndRenderPass,
                Enc::CopyBufferToBuffer { src: 1, src_offset: 0, dst: 3, dst_offset: 0, size: 16 },
            ],
            signal: Some((7, 42)),
        }),
        Cmd::Present { surface: 1, texture: 2 },
        Cmd::DestroyBuffer(1),
        Cmd::DestroyTexture(2),
    ];

    vec![
        ("create_buffer", create_buffer),
        ("write_buffer", write_buffer),
        ("create_texture", create_texture),
        ("command_buffer", command_buffer),
        ("present", present),
        ("destroy", destroy),
        ("full_stream", full_stream),
    ]
}

// -------------------------------------------------------------------------------------------------
// checked-in golden hex (lowercase, no separators) — produced by encode_stream over golden_cases()
// -------------------------------------------------------------------------------------------------

const GOLDEN_CREATE_BUFFER: &str = "010100000000010000000000003800000000000000";
const GOLDEN_WRITE_BUFFER: &str = "0301000000000000000000000004000000deadbeef";
const GOLDEN_CREATE_TEXTURE: &str =
    "0402000000040000000400000001000000010000000100000002000000010000000c00000000000000";
const GOLDEN_COMMAND_BUFFER: &str = "1303000000010100000002000000010000000000803f00000000000000000000803f0100020d010000000000000000000000030000000000000000000000100000000000000001070000002a00000000000000";
const GOLDEN_PRESENT: &str = "150100000002000000";
const GOLDEN_DESTROY: &str = "02010000000502000000";
const GOLDEN_FULL_STREAM: &str = "0101000000000100000000000038000000000000000301000000000000000000000004000000deadbeef0103000000000100000000000020000000000000000402000000040000000400000001000000010000000100000002000000010000002c000000000000001303000000010100000002000000010000000000803f00000000000000000000803f0100020d010000000000000000000000030000000000000000000000100000000000000001070000002a0000000000000015010000000200000002010000000502000000";

fn expected_hex(name: &str) -> &'static str {
    match name {
        "create_buffer" => GOLDEN_CREATE_BUFFER,
        "write_buffer" => GOLDEN_WRITE_BUFFER,
        "create_texture" => GOLDEN_CREATE_TEXTURE,
        "command_buffer" => GOLDEN_COMMAND_BUFFER,
        "present" => GOLDEN_PRESENT,
        "destroy" => GOLDEN_DESTROY,
        "full_stream" => GOLDEN_FULL_STREAM,
        other => panic!("no golden hex registered for case {other}"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn case(name: &str) -> Vec<Cmd> {
    golden_cases()
        .into_iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("no golden case named {name}"))
        .1
}

/// Assert one case both encodes to its frozen bytes AND round-trips through decode losslessly.
fn assert_golden(name: &str) {
    let cmds = case(name);
    let bytes = encode_stream(&cmds);
    assert_eq!(hex(&bytes), expected_hex(name), "encoded bytes for `{name}` drifted from the golden vector");
    let decoded = decode_stream(&bytes).expect("golden bytes must decode");
    assert_eq!(decoded, cmds, "decode(encode(x)) != x for `{name}`");
}

// -------------------------------------------------------------------------------------------------
// per-case golden + round-trip tests
// -------------------------------------------------------------------------------------------------

#[test]
fn golden_create_buffer() {
    assert_golden("create_buffer");
}

#[test]
fn golden_write_buffer() {
    assert_golden("write_buffer");
}

#[test]
fn golden_create_texture() {
    assert_golden("create_texture");
}

#[test]
fn golden_command_buffer_clear_and_copy() {
    assert_golden("command_buffer");
}

#[test]
fn golden_present() {
    assert_golden("present");
}

#[test]
fn golden_destroy() {
    assert_golden("destroy");
}

#[test]
fn golden_full_stream() {
    assert_golden("full_stream");
}

// -------------------------------------------------------------------------------------------------
// cross-cutting invariants
// -------------------------------------------------------------------------------------------------

/// Every registered case both matches its golden bytes and round-trips — belt-and-suspenders over the
/// individual tests, and it fails loudly if a case is added to `golden_cases` without a golden entry.
#[test]
fn all_cases_match_golden_and_round_trip() {
    for (name, cmds) in golden_cases() {
        let bytes = encode_stream(&cmds);
        assert_eq!(hex(&bytes), expected_hex(name), "golden byte drift in `{name}`");
        assert_eq!(decode_stream(&bytes).unwrap(), cmds, "round-trip failure in `{name}`");
    }
}

/// The per-command self-delimiting frame codec (`Cmd::frame` / `Cmd::decode_frame`) must also
/// round-trip every individual command in the corpus.
#[test]
fn every_command_frame_round_trips() {
    for (name, cmds) in golden_cases() {
        for (i, cmd) in cmds.iter().enumerate() {
            let framed = cmd.frame();
            let mut d = Decoder::new(&framed);
            let back = Cmd::decode_frame(&mut d).unwrap_or_else(|e| panic!("frame decode failed for `{name}`[{i}]: {e:?}"));
            assert!(d.is_empty(), "frame for `{name}`[{i}] left trailing bytes");
            assert_eq!(&back, cmd, "frame round-trip mismatch for `{name}`[{i}]");
        }
    }
}

/// Pin the current wire version explicitly. A bump must be a deliberate edit here (and will usually
/// coincide with regenerated golden bytes above), never an accidental drift.
#[test]
fn wire_version_is_pinned() {
    assert_eq!(WIRE_VERSION, 4, "WIRE_VERSION changed — update golden vectors and bump this pin deliberately");
}

/// A tiny defense that the hex snapshots are self-consistent: the full-stream vector must be the exact
/// concatenation of what its constituent commands encode to (encode_stream has no per-command framing).
#[test]
fn full_stream_is_backtoback_concatenation() {
    let full = case("full_stream");
    let mut expect = Vec::new();
    for cmd in &full {
        expect.extend_from_slice(&encode_stream(std::slice::from_ref(cmd)));
    }
    assert_eq!(encode_stream(&full), expect);
}
