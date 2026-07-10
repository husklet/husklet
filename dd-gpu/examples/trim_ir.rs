use dd_gpu::ir::{decode_stream, encode_stream, Cmd};

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .expect("usage: trim_ir <input> <output> <submit-op-count>");
    let output = args
        .next()
        .expect("usage: trim_ir <input> <output> <submit-op-count>");
    let keep_ops: usize = args
        .next()
        .expect("usage: trim_ir <input> <output> <submit-op-count>")
        .parse()
        .expect("submit-op-count must be numeric");

    let bytes = std::fs::read(&input).expect("read input");
    let mut cmds = decode_stream(&bytes).expect("decode input");
    let submit = cmds
        .iter_mut()
        .find_map(|cmd| match cmd {
            Cmd::Submit(cb) => Some(cb),
            _ => None,
        })
        .expect("input has no Submit command");
    submit.encoder.truncate(keep_ops.min(submit.encoder.len()));
    let kept = submit.encoder.len();
    let out = encode_stream(&cmds);
    std::fs::write(&output, &out).expect("write output");
    println!(
        "trim_ir: {} -> {} ops={} bytes={}",
        input,
        output,
        kept,
        out.len()
    );
}
