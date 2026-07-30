use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct VertexBinding {
    pub(super) buffer: u32,
    pub(super) bytes: usize,
    pub(super) offset: u64,
}

pub(super) struct VertexDraw<'a> {
    pub(super) pipeline: u32,
    pub(super) layouts: &'a [VertexLayout],
    pub(super) bindings: &'a [VertexBinding],
    pub(super) indexed: bool,
    pub(super) first_vertex: u32,
    pub(super) vertex_count: u32,
    pub(super) first_instance: u32,
    pub(super) instance_count: u32,
}

impl VertexDraw<'_> {
    fn format_bytes(format: u32) -> u64 {
        let components = u64::from(format & 0xff);
        let component = match (format >> 8) & 0xff {
            1 | 2 => 1,
            3 | 4 | 7 => 2,
            _ => 4,
        };
        components.saturating_mul(component)
    }

    fn limit(layout: &VertexLayout, binding: VertexBinding) -> u64 {
        let available = (binding.bytes as u64).saturating_sub(binding.offset);
        let attribute_end = layout
            .attrs
            .iter()
            .map(|attribute| u64::from(attribute.offset) + Self::format_bytes(attribute.format))
            .max()
            .unwrap_or(0);
        if available < attribute_end {
            return 0;
        }
        if layout.stride == 0 {
            return u64::MAX;
        }
        (available - attribute_end) / u64::from(layout.stride) + 1
    }

    pub(super) fn trace(&self) {
        for (slot, (layout, binding)) in self.layouts.iter().zip(self.bindings).enumerate() {
            let limit = Self::limit(layout, *binding);
            let requested = if layout.step_mode == 0 {
                u64::from(self.first_vertex) + u64::from(self.vertex_count)
            } else {
                u64::from(self.first_instance) + u64::from(self.instance_count)
            };
            if !self.indexed && requested > limit {
                if std::env::var_os("HL_SHIM_DEBUG").is_some() {
                    eprintln!(
                        "[hl-gl-shim] invalid vertex range pipeline={} slot={} buffer={} bytes={} \
                         offset={} stride={} step_mode={} indexed={} requested={} limit={} first_vertex={} \
                         vertex_count={} first_instance={} instance_count={} attrs={:?}",
                        self.pipeline,
                        slot,
                        binding.buffer,
                        binding.bytes,
                        binding.offset,
                        layout.stride,
                        layout.step_mode,
                        self.indexed,
                        requested,
                        limit,
                        self.first_vertex,
                        self.vertex_count,
                        self.first_instance,
                        self.instance_count,
                        layout.attrs
                    );
                }
                hl_log::hl_warn!(
                    hl_log::tag::GL,
                    "invalid vertex range pipeline={} slot={} buffer={} bytes={} offset={} stride={} \
                     step_mode={} requested={} limit={} first_vertex={} vertex_count={} first_instance={} \
                     instance_count={} attrs={:?}",
                    self.pipeline,
                    slot,
                    binding.buffer,
                    binding.bytes,
                    binding.offset,
                    layout.stride,
                    layout.step_mode,
                    requested,
                    limit,
                    self.first_vertex,
                    self.vertex_count,
                    self.first_instance,
                    self.instance_count,
                    layout.attrs
                );
            } else if self.first_vertex != 0 || binding.offset != 0 {
                hl_log::hl_debug!(
                    hl_log::tag::GL,
                    "vertex range pipeline={} slot={} buffer={} bytes={} offset={} stride={} step_mode={} \
                     requested={} limit={} first_vertex={} vertex_count={} first_instance={} \
                     instance_count={} attrs={:?}",
                    self.pipeline,
                    slot,
                    binding.buffer,
                    binding.bytes,
                    binding.offset,
                    layout.stride,
                    layout.step_mode,
                    requested,
                    limit,
                    self.first_vertex,
                    self.vertex_count,
                    self.first_instance,
                    self.instance_count,
                    layout.attrs
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(step_mode: u32) -> VertexLayout {
        VertexLayout {
            stride: 16,
            step_mode,
            attrs: vec![VertexAttr {
                location: 0,
                format: vertex_format_wire(GL_FLOAT, 2, false, false),
                offset: 0,
            }],
        }
    }

    #[test]
    fn limit_accounts_for_binding_offset_and_attribute_extent() {
        assert_eq!(
            VertexDraw::limit(
                &layout(0),
                VertexBinding {
                    buffer: 7,
                    bytes: 1_440,
                    offset: 800,
                }
            ),
            40
        );
    }

    #[test]
    fn instance_layout_uses_instance_range() {
        let layout = layout(1);
        let binding = VertexBinding {
            buffer: 7,
            bytes: 640,
            offset: 0,
        };
        let draw = VertexDraw {
            pipeline: 3,
            layouts: std::slice::from_ref(&layout),
            bindings: std::slice::from_ref(&binding),
            indexed: false,
            first_vertex: 50,
            vertex_count: 40,
            first_instance: 0,
            instance_count: 40,
        };

        assert_eq!(VertexDraw::limit(&layout, binding), 40);
        assert_eq!(
            if layout.step_mode == 0 {
                u64::from(draw.first_vertex) + u64::from(draw.vertex_count)
            } else {
                u64::from(draw.first_instance) + u64::from(draw.instance_count)
            },
            40
        );
    }
}
