use hl_gpu::ir::{decode_stream, BindResource, Cmd, Enc};
use std::collections::HashSet;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: dump_ir <ir-file> [buffer-id ...]");
    let inspect_buffers = args
        .filter_map(|s| s.parse::<u32>().ok())
        .collect::<HashSet<_>>();
    let bytes = std::fs::read(&path).expect("read ir");
    let cmds = decode_stream(&bytes).expect("decode ir");
    println!("bytes={} cmds={}", bytes.len(), cmds.len());
    for (i, cmd) in cmds.iter().enumerate() {
        match cmd {
            Cmd::CreateBuffer(id, desc) => {
                println!(
                    "{i:04} CreateBuffer id={id} size={} usage={}",
                    desc.size, desc.usage
                );
            }
            Cmd::WriteBuffer { id, offset, data } => {
                println!("{i:04} WriteBuffer id={id} off={offset} len={}", data.len());
                if data.len() <= 32 && data.len() % 4 == 0 {
                    let floats = data
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect::<Vec<_>>();
                    println!("      f32={floats:?}");
                }
                if inspect_buffers.contains(id) {
                    dump_buffer_head(data);
                }
            }
            Cmd::CreateTexture(id, desc) => {
                println!(
                    "{i:04} CreateTexture id={id} {}x{} fmt={:?} usage={}",
                    desc.width, desc.height, desc.format, desc.usage
                );
            }
            Cmd::CreateShader { id, kind, spirv } => {
                let len = spirv.first().copied().unwrap_or(0);
                println!("{i:04} CreateShader id={id} kind={kind:?} bytes={len}");
            }
            Cmd::CreateRenderPipeline(id, desc) => {
                println!(
                    "{i:04} CreatePipeline id={id} topo={:?} color={:?} vbufs={}",
                    desc.topology,
                    desc.color_targets.first().map(|t| (
                        &t.format,
                        t.blend.is_some(),
                        t.write_mask
                    )),
                    desc.vertex_buffers.len()
                );
                for (slot, layout) in desc.vertex_buffers.iter().enumerate() {
                    println!(
                        "      vb{slot} stride={} attrs={:?}",
                        layout.stride,
                        layout
                            .attrs
                            .iter()
                            .map(|a| (a.location, a.format, a.offset))
                            .collect::<Vec<_>>()
                    );
                }
            }
            Cmd::CreateBindGroup(id, desc) => {
                println!(
                    "{i:04} CreateBindGroup id={id} set={} entries={}",
                    desc.set,
                    desc.entries.len()
                );
                for e in &desc.entries {
                    let resource = match &e.resource {
                        BindResource::Buffer { id, offset, size } => {
                            format!("buffer id={id} off={offset} size={size}")
                        }
                        BindResource::Texture { id } => format!("texture id={id}"),
                        BindResource::Sampler { id } => format!("sampler id={id}"),
                    };
                    println!("      binding {} -> {}", e.binding, resource);
                }
            }
            Cmd::Submit(cb) => {
                println!("{i:04} Submit ops={}", cb.encoder.len());
                for (j, enc) in cb.encoder.iter().enumerate() {
                    match enc {
                        Enc::BeginRenderPass { color, .. } => {
                            let c = color.first();
                            println!(
                                "      {j:04} Begin target={:?} load={:?} clear={:?}",
                                c.map(|c| c.texture),
                                c.map(|c| &c.load),
                                c.map(|c| c.clear)
                            );
                        }
                        Enc::SetPipeline(id) => println!("      {j:04} SetPipeline {id}"),
                        Enc::SetBindGroup { index, group } => {
                            println!("      {j:04} SetBindGroup index={index} group={group}");
                        }
                        Enc::SetVertexBuffer {
                            slot,
                            buffer,
                            offset,
                        } => {
                            println!("      {j:04} SetVB slot={slot} buffer={buffer} off={offset}");
                        }
                        Enc::SetIndexBuffer {
                            buffer,
                            offset,
                            format,
                        } => {
                            println!(
                                "      {j:04} SetIB buffer={buffer} off={offset} fmt={format:?}"
                            );
                        }
                        Enc::SetViewport { x, y, w, h, .. } => {
                            println!("      {j:04} Viewport {x},{y} {w}x{h}");
                        }
                        Enc::SetScissor { x, y, w, h } => {
                            println!("      {j:04} Scissor {x},{y} {w}x{h}");
                        }
                        Enc::Draw {
                            vertex_count,
                            first_vertex,
                            ..
                        } => {
                            println!("      {j:04} Draw count={vertex_count} first={first_vertex}");
                        }
                        Enc::DrawIndexed {
                            index_count,
                            first_index,
                            ..
                        } => {
                            println!(
                                "      {j:04} DrawIndexed count={index_count} first={first_index}"
                            );
                        }
                        Enc::CopyBufferToTexture {
                            src,
                            bytes_per_row,
                            dst,
                            width,
                            height,
                            ..
                        } => {
                            println!(
                                "      {j:04} CopyBufferToTexture src={src} dst={dst} bpr={bytes_per_row} {width}x{height}"
                            );
                        }
                        Enc::EndRenderPass => println!("      {j:04} End"),
                        other => println!("      {j:04} {other:?}"),
                    }
                }
            }
            other => println!("{i:04} {other:?}"),
        }
    }
}

fn dump_buffer_head(data: &[u8]) {
    let n = data.len().min(160);
    println!("      first {n} bytes:");
    for (i, chunk) in data[..n].chunks(16).enumerate() {
        print!("        {:04x}:", i * 16);
        for b in chunk {
            print!(" {b:02x}");
        }
        println!();
    }
    let f32s = data[..n - (n % 4)]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect::<Vec<_>>();
    println!("      f32={f32s:?}");
    let u16s = data[..n - (n % 2)]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect::<Vec<_>>();
    println!("      u16={u16s:?}");
}
