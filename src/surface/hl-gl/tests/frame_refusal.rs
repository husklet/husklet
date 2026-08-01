//! A host-refused frame must be attributable to the GL objects that could have caused it.
//!
//! Translation failures are detected HOST-side, at frame submit, and the host refuses the frame whole.
//! The acknowledgement is one status byte, so the guest learns only that the frame was rejected — the
//! reason stays in the host's log. What the guest can still do is name the programs whose shaders it
//! asked the host to translate into that frame. Without that, the chain ends at "a frame vanished" and
//! the investigation starts at the renderer instead of at the shader.
//!
//! These cases pin the boundary between a diagnosis and a verdict: a refusal produces CANDIDATES and no
//! state change, and a transport that merely died produces nothing at all, because it never judged the
//! frame.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{record, swap};

use hl_gpu::protocol::model::capability::{Capabilities, FeatureRequest};
use hl_gpu::transport::model::error::{TransportError, TransportPhase};
use hl_gpu::{BufferId, Cmd, CommandSink, FenceId, GpuError, RecordingSink};

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str = "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0); }\n";

/// A sink that records every batch and then answers with one chosen failure — so a test can drive the
/// REAL frame path and choose whether the host refused the frame or the transport died under it.
struct FailingSink {
    inner: RecordingSink,
    failure: Box<dyn Fn() -> GpuError>,
}

impl FailingSink {
    /// A host that refused the frame, answering with `acknowledgement`. The value is the field the host
    /// is growing a reason class into, so a test may vary it; today any non-success value means the same
    /// thing to this driver, and that is asserted rather than assumed.
    fn refusing(acknowledgement: u8) -> Self {
        Self {
            inner: RecordingSink::with_full_caps(),
            failure: Box::new(move || {
                GpuError::Transport(TransportError::Rejected {
                    phase: TransportPhase::Acknowledgement,
                    acknowledgement,
                })
            }),
        }
    }

    fn dead() -> Self {
        Self {
            inner: RecordingSink::with_full_caps(),
            failure: Box::new(|| {
                GpuError::Transport(TransportError::Unavailable {
                    phase: TransportPhase::Acknowledgement,
                    detail: "socket closed".into(),
                })
            }),
        }
    }
}

impl CommandSink for FailingSink {
    fn negotiate(&mut self, request: &FeatureRequest) -> hl_gpu::Result<Capabilities> {
        self.inner.negotiate(request)
    }

    fn submit(&mut self, batch: &[Cmd]) -> hl_gpu::Result<()> {
        self.inner.submit(batch)?;
        Err((self.failure)())
    }

    fn wait(&mut self, fence: FenceId, value: u64) -> hl_gpu::Result<()> {
        self.inner.wait(fence, value)
    }

    fn read_buffer(&mut self, id: BufferId, offset: u64, len: usize) -> hl_gpu::Result<Vec<u8>> {
        self.inner.read_buffer(id, offset, len)
    }
}

fn ctx_64() -> GlContext {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: 64,
        height: 64,
    });
    c
}

/// A linked, bound program with a triangle to draw — the shape every frame in this file submits.
fn drawable(c: &mut GlContext) -> u32 {
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS);
    record::compile_shader(c, vs);
    let fs = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fs, FS);
    record::compile_shader(c, fs);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fs);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);
    let vbo = c.buffers.gen();
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(c, GL_ARRAY_BUFFER, &[0u8; 24], 0x88E4);
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(c, 0);
    record::draw_arrays(c, GL_TRIANGLES, 0, 3);
    prog
}

#[test]
fn a_refused_frame_names_the_programs_it_translated() {
    let mut c = ctx_64();
    let program = drawable(&mut c);

    // THE POSITIVE CONTROL, first: the same frame against a sink that accepts must submit, must carry a
    // CreateShader for this program, and must leave nothing refused. Without this the refusal below could
    // be a broken fixture refusing an empty frame.
    let mut accepting = RecordingSink::with_full_caps();
    let mut control = ctx_64();
    let control_program = drawable(&mut control);
    assert!(swap::swap_buffers(&mut control, &mut accepting).is_ok());
    let submitted: Vec<Cmd> = accepting.commands().cloned().collect();
    let candidates = control.refusal_candidates(&submitted);
    assert_eq!(
        candidates.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        vec![control_program, control_program],
        "an accepted frame translated both stages of exactly this program"
    );
    assert_eq!(
        control.refused_frames(),
        0,
        "nothing was refused on the control"
    );

    // Now the refusal. The frame is identical; only the host's answer differs.
    let mut sink = FailingSink::refusing(0);
    let outcome = swap::swap_buffers(&mut c, &mut sink);
    assert!(outcome.is_err(), "the host refused this frame");
    assert_eq!(
        c.refused_frames(),
        1,
        "the refusal is counted, so a report can say whether it happened once or every frame"
    );

    // Read the attribution the driver captured, NOT one reconstructed now: the frame's residency was
    // rolled back when the submit failed, so a caller asking afterwards would find the mapping gone.
    let candidates = c.last_refusal_candidates().to_vec();
    assert!(
        !candidates.is_empty(),
        "a refused frame that translated a shader must name where it came from"
    );
    assert!(
        candidates.iter().all(|(named, _)| *named == program),
        "and it must name the program the application knows: {candidates:?}"
    );

    // A diagnosis, not a verdict: the refusal names no command, so the program must NOT be condemned.
    assert_eq!(
        hl_gl::service::query::get_programiv(&c, program, GL_LINK_STATUS),
        GL_TRUE as i32,
        "one refused frame is not proof this program was the cause"
    );
    assert_eq!(hl_gl::service::query::program_info_log(&c, program), "");
}

#[test]
fn a_dead_transport_implicates_no_program() {
    let mut c = ctx_64();
    drawable(&mut c);

    let mut sink = FailingSink::dead();
    assert!(swap::swap_buffers(&mut c, &mut sink).is_err());
    assert!(
        c.last_refusal_candidates().is_empty(),
        "a dead transport implicates nobody"
    );
    assert_eq!(
        c.refused_frames(),
        0,
        "a transport that died never judged the frame, so no program is implicated by it — \
         attributing a dropped socket to a shader is the escalation mistake pointed the other way"
    );
}

#[test]
fn a_refused_frame_that_translated_nothing_names_nobody() {
    let mut c = ctx_64();
    let program = drawable(&mut c);
    // First frame translates the program's two stages and is accepted, so its shader modules are
    // resident; the second frame reuses them and introduces no new translation.
    let mut accepting = RecordingSink::with_full_caps();
    assert!(swap::swap_buffers(&mut c, &mut accepting).is_ok());
    assert!(
        !c.refusal_candidates(&accepting.commands().cloned().collect::<Vec<_>>())
            .is_empty(),
        "the first frame must have translated something, or the second proves nothing"
    );

    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    let mut sink = FailingSink::refusing(0);
    assert!(swap::swap_buffers(&mut c, &mut sink).is_err());
    assert!(
        c.last_refusal_candidates().is_empty(),
        "this frame added no translation, so the refusal was about something else — \
         claiming a candidate here would send the next reader to the translator for nothing"
    );
    assert_eq!(c.refused_frames(), 1);
    let _ = program;
}

/// The acknowledgement byte is growing one value per host error kind, so this driver must key on the
/// REFUSAL rather than on the particular value. A driver that recognised only today's value would stop
/// attributing the moment the host began classifying — and worse, an unrecognised refusal would fall to
/// the transport-death side of the classification and retire the whole share group over one bad frame.
#[test]
fn any_refusal_value_is_still_a_refusal() {
    for acknowledgement in [0u8, 2, 3, 7, 255] {
        let mut c = ctx_64();
        let program = drawable(&mut c);
        let mut sink = FailingSink::refusing(acknowledgement);
        assert!(
            swap::swap_buffers(&mut c, &mut sink).is_err(),
            "ack={acknowledgement}"
        );
        assert_eq!(
            c.refused_frames(),
            1,
            "ack={acknowledgement} must count as a refusal"
        );
        assert!(
            c.last_refusal_candidates()
                .iter()
                .all(|(named, _)| *named == program),
            "ack={acknowledgement} must still attribute: {:?}",
            c.last_refusal_candidates()
        );
        assert!(
            !c.last_refusal_candidates().is_empty(),
            "ack={acknowledgement} named nobody"
        );
    }
}
