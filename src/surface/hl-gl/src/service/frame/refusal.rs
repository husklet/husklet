//! Attribute a host-refused frame back to the GL objects that could have caused it.
//!
//! A shader that fails to translate is refused by the HOST, at frame submit, and the host refuses the
//! frame WHOLE. The guest is told only that the frame was rejected: the acknowledgement is a single
//! status byte, so the reason the host wrote down — naga's actual message, logged host-side as
//! `verdict=nack error=…` — never crosses the wire. What the guest does still know is which shader
//! modules it asked the host to create in that frame, and which GL program and specialisation each of
//! them came from. That is enough to point at the objects worth looking at, which is the difference
//! between a frame that vanished and a frame that vanished BECAUSE of program 7.
//!
//! Deliberately a diagnosis and not a verdict. A refusal names no command, so every shader newly
//! translated in the frame is a CANDIDATE and none of them is proven; marking a program unlinked on that
//! basis would let one bad buffer upload condemn a program that translates perfectly well. When the
//! acknowledgement grows a reason — the shared wire change the Vulkan surface needs for the same reason
//! this one does — the candidate set collapses to the command the host named, and only then can this
//! legitimately write the program's info log.

use crate::model::context::GlContext;
use hl_gpu::{Cmd, GpuError};

impl GlContext {
    /// The `(program, variant)` pairs a refused batch could be blaming: every shader module the batch
    /// asked the host to create, resolved back through the residency cache to the GL program and
    /// specialisation it was translated for. Empty when the batch introduced no new translation, which is
    /// itself an answer — the refusal was about something other than a shader this frame added.
    pub fn refusal_candidates(&self, cmds: &[Cmd]) -> Vec<(u32, u64)> {
        cmds.iter()
            .filter_map(|command| match command {
                Cmd::CreateShader { id, .. } => self.shader_ir_program(*id),
                _ => None,
            })
            .collect()
    }

    /// How many frames the host has refused on this context.
    pub fn refused_frames(&self) -> u64 {
        self.local.refused_frames
    }

    /// The `(program, variant)` pairs implicated by the most recent refusal, captured before the frame's
    /// residency was rolled back. Empty when nothing has been refused, and empty when the refused frame
    /// introduced no new translation — which is an answer, not an absence of one.
    pub fn last_refusal_candidates(&self) -> &[(u32, u64)] {
        &self.local.refusal_candidates
    }
}

/// Report the programs whose translation the host may have refused, on a doubling cadence.
///
/// A broken program is redrawn every frame, so an unconditional report would flood a shipped log; a
/// latched one could not tell "once at startup" from "every frame since". Reporting at each power of two
/// with the running count says which it is in bounded space.
pub(crate) fn report(ctx: &mut GlContext, error: &GpuError, cmds: &[Cmd]) {
    // Only a REFUSAL is attributable. A transport that is gone, ambiguous or retired did not read the
    // frame, let alone judge it — blaming a program for a dropped socket would be the same mistake as
    // the escalation that used to kill the share group over one bad batch.
    let GpuError::Transport(failure) = error else {
        return;
    };
    if !failure.refusal() {
        return;
    }
    // The acknowledgement value VERBATIM. Today it carries only "not success", so it says nothing beyond
    // the fact of the refusal — but it is the field the host is growing a reason CLASS into, one value per
    // error kind it already holds typed. Printing it now means the class appears in this line the day it
    // lands, and until then an operator can still tell two different refusal values apart.
    let acknowledgement = match failure {
        hl_gpu::transport::model::error::TransportError::Rejected {
            acknowledgement, ..
        } => *acknowledgement,
        _ => 0,
    };
    ctx.local.refused_frames = ctx.local.refused_frames.saturating_add(1);
    let count = ctx.local.refused_frames;

    let created = cmds
        .iter()
        .filter(|command| matches!(command, Cmd::CreateShader { .. }))
        .count();
    // Taken NOW: the caller rolls this frame's residency back the moment we return, and with it the only
    // mapping from the refused shader modules to the programs they were translated for.
    ctx.local.refusal_candidates = ctx.refusal_candidates(cmds);
    let candidates = ctx
        .local
        .refusal_candidates
        .iter()
        .map(|(program, variant)| format!("program={program} variant={variant:#018x}"))
        .collect::<Vec<_>>();

    if !count.is_power_of_two() {
        return;
    }
    if candidates.is_empty() {
        // A refused frame that translated nothing new says something too: whatever the host objected to,
        // it was not a shader this frame introduced. Saying so keeps the next reader from re-auditing the
        // translator over a refusal that had nothing to do with it.
        hl_log::hl_error!(
            hl_log::tag::GL,
            "host REFUSED this frame and it created no new shader modules, so no program of this \
             context is implicated. cmds={} count={count}. The host log records the reason \
             (`verdict=nack error=…`); this side cannot see it. Reported at each power of two.",
            cmds.len()
        );
        return;
    }
    hl_log::hl_error!(
        hl_log::tag::GL,
        "host REFUSED this frame; these programs had a shader TRANSLATED into it and are the \
         candidates: [{}]. The refusal names no command, so this is the set to examine and not a \
         verdict on any one of them — and a variant is one specialisation of a program, not the \
         program, so its other specialisations may translate. cmds={} new_shaders={} \
         ack={acknowledgement} count={count}. The reason itself is in the HOST log \
         (`verdict=nack error=…`); the acknowledgement carries only its class. Reported at each power of \
         two.",
        candidates.join(", "),
        cmds.len(),
        created
    );
}
