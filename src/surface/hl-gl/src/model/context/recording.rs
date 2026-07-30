use crate::model::program::DrawCall;

use super::{BlitOp, FrameOp};

/// Deferred commands recorded by one GL context.
///
/// `operations` preserves cross-kind call order. `draws` and `blits` are parallel indexes used by
/// lowering and queries; mutations must keep all three collections coherent.
#[derive(Default)]
pub(crate) struct Recording {
    pub(crate) draws: Vec<DrawCall>,
    pub(crate) blits: Vec<BlitOp>,
    pub(crate) operations: Vec<FrameOp>,
}

impl Recording {
    pub(super) fn clear(&mut self) {
        self.draws.clear();
        self.blits.clear();
        self.operations.clear();
    }

    pub(super) fn push_draw(&mut self, draw: DrawCall) {
        self.operations.push(FrameOp::Draw(Box::new(draw.clone())));
        self.draws.push(draw);
    }

    pub(super) fn push_blit(&mut self, blit: BlitOp) {
        self.operations.push(FrameOp::Blit(blit));
        self.blits.push(blit);
    }

    pub(super) fn replace_last_draw_program(&mut self, program: u32) -> bool {
        let Some(draw) = self.draws.last_mut() else {
            return false;
        };
        draw.prog = program;

        let Some(FrameOp::Draw(ordered)) = self
            .operations
            .iter_mut()
            .rev()
            .find(|operation| matches!(operation, FrameOp::Draw(_)))
        else {
            debug_assert!(false, "draw index and ordered operations diverged");
            return false;
        };
        ordered.prog = program;
        true
    }

    pub(super) fn references_program(&self, program: u32) -> bool {
        self.draws.iter().any(|draw| draw.prog == program)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_keeps_parallel_indexes_coherent() {
        let mut recording = Recording::default();
        let blit = BlitOp {
            read_fbo: 1,
            draw_fbo: 2,
            read_target: None,
            draw_target: None,
            read_ir: None,
            draw_ir: None,
            src: [0; 4],
            dst: [0; 4],
            filter: hl_gpu::protocol::model::enums::Filter::Nearest,
        };
        recording.push_blit(blit);
        assert_eq!(recording.blits, [blit]);
        assert_eq!(recording.operations, [FrameOp::Blit(blit)]);

        recording.clear();

        assert!(recording.draws.is_empty());
        assert!(recording.blits.is_empty());
        assert!(recording.operations.is_empty());
    }

    #[test]
    fn replacing_a_draw_updates_the_ordered_operation() {
        let mut recording = Recording::default();
        recording.push_draw(DrawCall::default());

        assert!(recording.replace_last_draw_program(7));
        assert_eq!(recording.draws[0].prog, 7);
        assert!(matches!(
            &recording.operations[0],
            FrameOp::Draw(draw) if draw.prog == 7
        ));
    }
}
