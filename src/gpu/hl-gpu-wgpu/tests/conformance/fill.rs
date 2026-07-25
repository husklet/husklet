use super::*;

// -------------------------------------------------------------------------------------------------
// FillBuffer — device-side memset
// -------------------------------------------------------------------------------------------------

#[test]
fn fill_buffer_writes_repeating_pattern() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::FillBuffer {
                    buffer: 1,
                    offset: 0,
                    size: 8,
                    value: 0xAABB_CCDD,
                }],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 8).unwrap();
    assert_eq!(out, [0xDD, 0xCC, 0xBB, 0xAA, 0xDD, 0xCC, 0xBB, 0xAA]);
}

#[test]
fn fill_buffer_scopes_to_offset_and_size() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![0xFF; 8],
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::FillBuffer {
                    buffer: 1,
                    offset: 2,
                    size: 3,
                    value: 0xAABB_CCDD,
                }],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 8).unwrap();
    assert_eq!(out, [0xFF, 0xFF, 0xDD, 0xCC, 0xBB, 0xFF, 0xFF, 0xFF]);
}
