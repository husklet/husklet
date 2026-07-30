//! Typed, message-carrying failures for the shader-translation seam: a program outside the supported
//! subset becomes a clean `GpuError::Kernel` diagnostic, never a silent wrong-shader substitution.
//!
//! The GLSL reporter also dumps the original and post-normalization sources, bounded in size, because
//! naga's identifier-only parse errors are otherwise impossible to reproduce from a log.

use hl_gpu::GpuError;

#[hl_design::naming(reason = "diagnostic is the established noun for a shader compiler report")]
pub(super) struct Diagnostic;

impl Diagnostic {
    pub(super) const GLSL_CONTEXT_LIMIT: usize = 4096;

    pub(super) fn kernel(message: impl Into<String>) -> GpuError {
        // The kernel/shader lowering surfaces its failures as a typed, message-carrying error so a program
        // outside the supported subset is a clean diagnostic, never a silent wrong-shader substitution.
        GpuError::Kernel(message.into())
    }

    pub(super) fn glsl(
        stage: naga::ShaderStage,
        entry: &str,
        original: &str,
        normalized: &str,
        error: &naga::front::glsl::ParseErrors,
    ) -> GpuError {
        // Shader failures are rare and otherwise impossible to reproduce from naga's identifier-only
        // diagnostic. Emit the original and exact post-normalization inputs only on failure; successful
        // Chrome frames add no logging or formatting cost.
        hl_log::hl_error!(
            hl_log::tag::WGPU,
            "GLSL translation failed stage={stage:?} entry={entry} error={error:?}\n\
             --- original GLSL begin ---\n{original}\n--- original GLSL end ---\n\
             --- normalized GLSL begin ---\n{normalized}\n--- normalized GLSL end ---"
        );
        Self::kernel(Self::glsl_message(original, normalized, error))
    }

    pub(super) fn glsl_message(
        original: &str,
        normalized: &str,
        error: &naga::front::glsl::ParseErrors,
    ) -> String {
        let mut message = String::new();
        Self::push_bounded(&mut message, &format!("glsl-in: {error:?}"));
        let names = error
            .errors
            .iter()
            .filter_map(|error| match &error.kind {
                naga::front::glsl::ErrorKind::UnknownVariable(name) => Some(name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if names.is_empty() {
            return message;
        }
        Self::source_matches(&mut message, "original", original, &names);
        Self::source_matches(&mut message, "normalized", normalized, &names);
        message
    }

    fn source_matches(output: &mut String, label: &str, source: &str, names: &[&str]) {
        if output.len() >= Self::GLSL_CONTEXT_LIMIT {
            return;
        }
        let lines = source.lines().collect::<Vec<_>>();
        let mut selected = vec![false; lines.len()];
        for (index, line) in lines.iter().enumerate() {
            if names.iter().any(|name| line.contains(name)) {
                let start = index.saturating_sub(1);
                let end = (index + 2).min(lines.len());
                selected[start..end].fill(true);
            }
        }
        if !selected.iter().any(|selected| *selected) {
            return;
        }
        Self::push_bounded(output, &format!("\n{label} GLSL context:"));
        for (index, line) in lines.iter().enumerate() {
            if selected[index] {
                Self::push_bounded(output, &format!("\n{:>5} | {line}", index + 1));
            }
        }
    }

    fn push_bounded(output: &mut String, value: &str) {
        let remaining = Self::GLSL_CONTEXT_LIMIT.saturating_sub(output.len());
        if remaining == 0 {
            return;
        }
        let mut end = remaining.min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        output.push_str(&value[..end]);
    }
}
