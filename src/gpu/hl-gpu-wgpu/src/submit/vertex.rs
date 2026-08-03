use super::*;
use hl_gpu::protocol::model::descriptor::VertexLayout;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Binding {
    pub(super) buffer: u32,
    pub(super) size: u64,
    pub(super) offset: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Violation {
    pub(super) slot: u32,
    pub(super) binding: Binding,
    pub(super) requested: u64,
    pub(super) limit: u64,
}

pub(super) struct Draw<'a> {
    pub(super) pipeline: u32,
    pub(super) layouts: &'a [VertexLayout],
    pub(super) bindings: &'a BTreeMap<u32, Binding>,
    pub(super) first_vertex: u32,
    pub(super) vertex_count: u32,
    pub(super) first_instance: u32,
    pub(super) instance_count: u32,
}

impl Draw<'_> {
    fn format_size(format: u32) -> u64 {
        if (format >> 8) & 0xff == 8 {
            return 4;
        }
        let components = u64::from(format & 0xff);
        let component = match (format >> 8) & 0xff {
            1 | 2 | 9 => 1,
            3 | 4 | 7 => 2,
            _ => 4,
        };
        components.saturating_mul(component)
    }

    fn limit(layout: &VertexLayout, binding: Binding) -> u64 {
        let available = binding.size.saturating_sub(binding.offset);
        let attribute_end = layout
            .attrs
            .iter()
            .map(|attribute| u64::from(attribute.offset) + Self::format_size(attribute.format))
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

    pub(super) fn violations(&self) -> Vec<Violation> {
        self.layouts
            .iter()
            .enumerate()
            .filter_map(|(slot, layout)| {
                let binding = *self.bindings.get(&(slot as u32))?;
                let requested = if layout.step_mode == 0 {
                    u64::from(self.first_vertex) + u64::from(self.vertex_count)
                } else {
                    u64::from(self.first_instance) + u64::from(self.instance_count)
                };
                let limit = Self::limit(layout, binding);
                (requested > limit).then_some(Violation {
                    slot: slot as u32,
                    binding,
                    requested,
                    limit,
                })
            })
            .collect()
    }

    pub(super) fn log(&self) {
        for violation in self.violations() {
            let layout = &self.layouts[violation.slot as usize];
            hl_log::hl_warn!(
                tag::EXEC,
                "vertex draw invalid pipeline={} slot={} buffer={} size={} offset={} stride={} \
                 step_mode={} requested={} limit={} first_vertex={} vertex_count={} first_instance={} \
                 instance_count={} attrs={:?}",
                self.pipeline,
                violation.slot,
                violation.binding.buffer,
                violation.binding.size,
                violation.binding.offset,
                layout.stride,
                layout.step_mode,
                violation.requested,
                violation.limit,
                self.first_vertex,
                self.vertex_count,
                self.first_instance,
                self.instance_count,
                layout.attrs
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_gpu::protocol::model::descriptor::VertexAttr;

    fn layout(step_mode: u32) -> VertexLayout {
        VertexLayout {
            stride: 16,
            step_mode,
            attrs: vec![VertexAttr {
                location: 0,
                format: 2,
                offset: 0,
            }],
        }
    }

    #[test]
    fn identifies_exact_vertex_ninety_over_limit_forty_tuple() {
        let binding = Binding {
            buffer: 71,
            size: 640,
            offset: 0,
        };
        let bindings = BTreeMap::from([(0, binding)]);
        let layouts = [layout(0)];
        let draw = Draw {
            pipeline: 19,
            layouts: &layouts,
            bindings: &bindings,
            first_vertex: 50,
            vertex_count: 40,
            first_instance: 0,
            instance_count: 1,
        };

        assert_eq!(
            draw.violations(),
            vec![Violation {
                slot: 0,
                binding,
                requested: 90,
                limit: 40,
            }]
        );
    }

    #[test]
    fn instance_layout_uses_instance_range_instead_of_vertex_range() {
        let bindings = BTreeMap::from([(
            0,
            Binding {
                buffer: 71,
                size: 640,
                offset: 0,
            },
        )]);
        let layouts = [layout(1)];
        let draw = Draw {
            pipeline: 19,
            layouts: &layouts,
            bindings: &bindings,
            first_vertex: 50,
            vertex_count: 40,
            first_instance: 0,
            instance_count: 40,
        };

        assert!(draw.violations().is_empty());
    }
}
