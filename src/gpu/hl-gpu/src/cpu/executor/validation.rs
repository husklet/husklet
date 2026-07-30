use super::{operation::validate_op, *};

/// Simulated encoder state used by the validation pass.
#[derive(Default)]
pub(super) struct EncoderState {
    pub(super) in_render_pass: bool,
    pub(super) in_compute_pass: bool,
    pub(super) pipeline: Option<u32>,
    pub(super) bind_group: Option<u32>,
    pub(super) vertex_buffers: HashMap<u32, (u32, u64)>,
    pub(super) index_buffer: Option<(u32, u64, IndexFormat)>,
    pub(super) color_targets: Vec<u32>,
    pub(super) color_formats: Vec<TextureFormat>,
}

impl EncoderState {
    pub(super) fn validate(resources: &SessionResources, commands: &CommandBuffer) -> Result<()> {
        let mut state = Self::default();
        for command in &commands.encoder {
            validate_op(resources, command, &mut state)?;
        }
        if state.in_render_pass || state.in_compute_pass {
            return Err(GpuError::Invalid("command buffer ends inside an open pass"));
        }
        // The completion signal is part of the command buffer, so its fence must resolve HERE. Executing
        // the ops first and only then failing on an unknown fence would leave the ops' writes behind: the
        // runtime transaction restores the id tables but cannot un-write pixels or buffer bytes.
        if let Some((fence, _)) = commands.signal {
            resources.fences.get(fence)?;
        }
        Ok(())
    }

    pub(super) fn end_pass(&mut self) {
        self.in_render_pass = false;
        self.in_compute_pass = false;
        self.color_targets.clear();
        self.color_formats.clear();
    }
}

pub(super) fn buffer_with_usage<'a>(
    res: &'a SessionResources,
    id: u32,
    usage: u32,
    what: &'static str,
) -> Result<&'a Buffer> {
    let b = buffer(res, id)?;
    if b.usage & usage == 0 {
        return Err(GpuError::Invalid(what));
    }
    Ok(b)
}

pub(super) fn texture_with_usage<'a>(
    res: &'a SessionResources,
    id: u32,
    usage: u32,
    what: &'static str,
) -> Result<&'a Texture> {
    let t = texture(res, id)?;
    if t.desc.usage & usage == 0 {
        return Err(GpuError::Invalid(what));
    }
    Ok(t)
}

pub(super) fn check_vertex_range(
    res: &SessionResources,
    buffer_id: u32,
    offset: u64,
    stride: u32,
    count: Option<u32>,
) -> Result<()> {
    let b = buffer(res, buffer_id)?;
    let count = count.ok_or(GpuError::OutOfBounds)?;
    let need = (count as u64)
        .checked_mul(stride as u64)
        .ok_or(GpuError::OutOfBounds)?;
    let end = offset.checked_add(need).ok_or(GpuError::OutOfBounds)?;
    if end > b.data.len() as u64 {
        return Err(GpuError::OutOfBounds);
    }
    Ok(())
}

pub(super) fn validate_draw<F>(
    res: &SessionResources,
    st: &EncoderState,
    instance_count: u32,
    mut per_layout: F,
) -> Result<()>
where
    F: FnMut(&crate::protocol::model::descriptor::VertexLayout, u32) -> Result<()>,
{
    if !st.in_render_pass {
        return Err(GpuError::Invalid("draw outside a render pass"));
    }
    if instance_count > crate::runtime::model::session::Limits::MAX_DRAW_INSTANCES {
        return Err(GpuError::ResourceLimit("draw instances"));
    }
    let pid = st
        .pipeline
        .ok_or(GpuError::Invalid("draw with no pipeline bound"))?;
    let (color_formats, vertex_layouts) = match crate::cpu::model::pipeline(res, pid)? {
        Pipeline::Render {
            color_formats,
            vertex_layouts,
            ..
        } => (color_formats.clone(), vertex_layouts.clone()),
        Pipeline::Compute { .. } => {
            return Err(GpuError::Unsupported("draw on a compute pipeline"))
        }
    };
    if color_formats.len() != st.color_formats.len()
        || color_formats
            .iter()
            .zip(&st.color_formats)
            .any(|(a, b)| a != b)
    {
        return Err(GpuError::Invalid(
            "pipeline color format mismatches render attachment",
        ));
    }
    for (slot, layout) in vertex_layouts.iter().enumerate() {
        per_layout(layout, slot as u32)?;
    }
    if let Some(bg) = st.bind_group {
        let bg = bind_group(res, bg)?;
        bg.validate(res)?;
        for r in &bg.textures {
            if st.color_targets.contains(&r.id) {
                return Err(GpuError::Invalid(
                    "texture sampled while bound as a color attachment",
                ));
            }
        }
    }
    Ok(())
}
