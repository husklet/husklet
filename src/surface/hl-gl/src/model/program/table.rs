//! Shader and program object collections.

use super::{Program, Shader};
use std::collections::HashMap;

/// The per-context shader + program tables. Shader and program objects share one name namespace; name
/// `0` is the reserved sentinel.
#[derive(Debug, Default)]
pub struct Programs {
    shaders: HashMap<u32, Shader>,
    programs: HashMap<u32, Program>,
    next_object: u32,
}

impl Programs {
    pub fn new() -> Self {
        Self {
            shaders: HashMap::new(),
            programs: HashMap::new(),
            next_object: 1,
        }
    }

    /// `glCreateShader(kind)` — mint a shader name.
    pub fn create_shader(&mut self, kind: u32) -> u32 {
        let name = self.next_object;
        self.next_object += 1;
        self.shaders.insert(
            name,
            Shader {
                kind,
                ..Default::default()
            },
        );
        name
    }

    /// `glShaderSource(shader, src)`.
    pub fn shader_source(&mut self, name: u32, src: &str) {
        if let Some(sh) = self.shaders.get_mut(&name) {
            sh.src = Some(src.to_string());
        }
    }

    /// `glCompileShader(shader)`.
    pub fn compile_shader(&mut self, name: u32) {
        if let Some(sh) = self.shaders.get_mut(&name) {
            sh.compiled = true;
            sh.info_log.clear();
        }
    }

    /// Refuse a compile, retaining the diagnostic for `glGetShaderInfoLog`.
    pub fn fail_compile(&mut self, name: u32, reason: String) {
        if let Some(sh) = self.shaders.get_mut(&name) {
            sh.compiled = false;
            sh.info_log = reason;
        }
    }

    /// The diagnostic from this shader's most recent refused compile (empty when it compiled).
    pub fn shader_info_log(&self, name: u32) -> &str {
        self.shaders
            .get(&name)
            .map(|sh| sh.info_log.as_str())
            .unwrap_or("")
    }

    /// Whether `name` is a live SHADER object (not a program, not a reclaimed name).
    pub fn has_shader(&self, name: u32) -> bool {
        self.shaders.contains_key(&name)
    }

    pub fn shader(&self, name: u32) -> Option<&Shader> {
        self.shaders.get(&name)
    }

    /// `glCreateProgram()` — mint a program name.
    pub fn create(&mut self) -> u32 {
        let name = self.next_object;
        self.next_object += 1;
        self.programs.insert(name, Program::default());
        name
    }

    /// `glAttachShader(program, shader)` — record the attachment by the shader's kind.
    pub fn attach(&mut self, program: u32, shader: u32) -> bool {
        let kind = self.shaders.get(&shader).map(|s| s.kind).unwrap_or(0);
        let Some(p) = self.programs.get_mut(&program) else {
            return false;
        };
        let slot = match kind {
            crate::model::glconst::GL_VERTEX_SHADER => &mut p.vs,
            crate::model::glconst::GL_FRAGMENT_SHADER => &mut p.fs,
            crate::model::glconst::GL_COMPUTE_SHADER => &mut p.cs,
            _ => return false,
        };
        if *slot != 0 {
            return false;
        }
        *slot = shader;
        true
    }

    /// Why an attached shader bars this program from linking, if one does.
    ///
    /// ES 3.0 §7.3: a link succeeds only when every attached shader carries `GL_COMPILE_STATUS` true.
    /// The status is read HERE rather than remembered from an earlier link, because a shader may be
    /// attached after a link and recompiled between two of them. A shader that was never compiled and
    /// one whose compile was refused are distinct states (ES 3.0 §7.1) and say so; the refused one
    /// repeats its own info log, which is the only place the actual cause is written down.
    fn attachment_barring_link(&self, program: u32) -> Option<String> {
        let program = self.programs.get(&program)?;
        let stages = [
            ("vertex", program.vs),
            ("fragment", program.fs),
            ("compute", program.cs),
        ];
        stages.into_iter().find_map(|(stage, name)| {
            if name == 0 {
                return None;
            }
            match self.shaders.get(&name) {
                None => Some(format!(
                    "attached {stage} shader {name} is not a shader object"
                )),
                Some(shader) if shader.compiled => None,
                Some(shader) if shader.info_log.is_empty() => {
                    Some(format!("attached {stage} shader {name} was never compiled"))
                }
                Some(shader) => Some(format!(
                    "attached {stage} shader {name} failed to compile: {}",
                    shader.info_log
                )),
            }
        })
    }

    /// `glLinkProgram(program)` — translate + reflect the attached shaders. Returns `false` if the
    /// program name or either attached shader is unknown, or if an attached shader did not compile.
    pub fn link(&mut self, program: u32) -> bool {
        if let Some(reason) = self.attachment_barring_link(program) {
            if let Some(p) = self.programs.get_mut(&program) {
                p.linked = false;
                p.link_error = reason;
            }
            return false;
        }
        let (vs, fs, cs) = match self.programs.get(&program) {
            Some(p) => (p.vs, p.fs, p.cs),
            None => return false,
        };
        let vs_src = self
            .shaders
            .get(&vs)
            .and_then(|s| s.src.clone())
            .unwrap_or_default();
        let fs_src = self
            .shaders
            .get(&fs)
            .and_then(|s| s.src.clone())
            .unwrap_or_default();
        let cs_src = self
            .shaders
            .get(&cs)
            .and_then(|s| s.src.clone())
            .unwrap_or_default();
        // ES 3.0 §7.3: a non-compute program links only when it has BOTH a vertex and a fragment shader
        // whose source compiled. Linking one stage alone used to succeed and report GL_LINK_STATUS = 1,
        // handing the app a program that can never draw.
        if cs_src.is_empty() && (vs_src.is_empty() || fs_src.is_empty()) {
            if let Some(p) = self.programs.get_mut(&program) {
                p.linked = false;
                p.link_error =
                    "link requires both a vertex and a fragment shader with compiled source".into();
            }
            return false;
        }
        if let Some(p) = self.programs.get_mut(&program) {
            p.linked = false;
            match p.link(vs_src, fs_src, cs_src) {
                Ok(()) => true,
                Err(error) => {
                    p.link_error = error.to_string();
                    false
                }
            }
        } else {
            false
        }
    }

    pub fn program(&self, name: u32) -> Option<&Program> {
        self.programs.get(&name)
    }

    pub fn get_mut(&mut self, name: u32) -> Option<&mut Program> {
        self.programs.get_mut(&name)
    }

    pub fn bind_attrib(&mut self, program: u32, index: u32, name: &str) -> bool {
        let Some(program) = self.programs.get_mut(&program) else {
            return false;
        };
        program.attrib_bindings.insert(name.to_owned(), index);
        true
    }

    /// `glIsProgram(name)` — true once `name` names a live program object (`0` is never a program).
    pub fn contains(&self, name: u32) -> bool {
        name != 0 && self.programs.contains_key(&name)
    }

    /// `glIsShader(name)` — true once `name` names a live shader object.
    pub fn shader_exists(&self, name: u32) -> bool {
        name != 0 && self.shaders.contains_key(&name)
    }

    /// Remove the program object outright (deleting `0` is a silent no-op; GL defines it so). The
    /// deferred-deletion rule of ES 3.0 §7.3 lives one layer up in
    /// [`crate::model::context::GlContext::delete_program`]; this is the unconditional removal it calls once
    /// the program is no longer current. Returns `false` for an unknown / zero name.
    pub fn delete(&mut self, name: u32) -> bool {
        if name == 0 {
            return false;
        }
        let Some(program) = self.programs.remove(&name) else {
            return false;
        };
        for shader in [program.vs, program.fs, program.cs] {
            if self.shader_flagged(shader) && !self.shader_attached(shader) {
                self.shaders.remove(&shader);
            }
        }
        true
    }

    /// Set `GL_DELETE_STATUS` on a program, leaving the object live (ES 3.0 §7.3). Returns `false` for an
    /// unknown / zero name or one already flagged.
    pub fn flag(&mut self, name: u32) -> bool {
        match self.programs.get_mut(&name) {
            Some(program) if !program.pending_delete => {
                program.pending_delete = true;
                true
            }
            _ => false,
        }
    }

    /// `GL_DELETE_STATUS` of a program.
    pub fn flagged(&self, name: u32) -> bool {
        self.programs
            .get(&name)
            .is_some_and(|program| program.pending_delete)
    }

    /// `GL_DELETE_STATUS` of a shader.
    pub fn shader_flagged(&self, name: u32) -> bool {
        self.shaders
            .get(&name)
            .is_some_and(|shader| shader.pending_delete)
    }

    /// True while any program still names `shader` in an attachment slot.
    pub fn shader_attached(&self, shader: u32) -> bool {
        shader != 0
            && self
                .programs
                .values()
                .any(|p| p.vs == shader || p.fs == shader || p.cs == shader)
    }

    /// `glDeleteShader(name)` — ES 3.0 §7.1: a shader still attached to a program is only FLAGGED for
    /// deletion and keeps its source and compile status, so the program can still relink from it; an
    /// unattached shader is removed at once. Deleting `0` is a silent no-op. Returns `false` for an
    /// unknown / zero name.
    pub fn delete_shader(&mut self, name: u32) -> bool {
        if name == 0 || !self.shaders.contains_key(&name) {
            return false;
        }
        if self.shader_attached(name) {
            if let Some(shader) = self.shaders.get_mut(&name) {
                shader.pending_delete = true;
            }
            return true;
        }
        self.shaders.remove(&name).is_some()
    }

    /// `glDetachShader(program, shader)` — clear the matching attachment slot, then reap the shader if that
    /// was its last attachment and it was already flagged for deletion (ES 3.0 §7.1). Returns `false` if
    /// the program is unknown or `shader` is not attached (the caller raises the spec error).
    pub fn detach(&mut self, program: u32, shader: u32) -> bool {
        let detached = match self.programs.get_mut(&program) {
            Some(p) if p.vs == shader => {
                p.vs = 0;
                true
            }
            Some(p) if p.fs == shader => {
                p.fs = 0;
                true
            }
            Some(p) if p.cs == shader => {
                p.cs = 0;
                true
            }
            _ => false,
        };
        if detached && self.shader_flagged(shader) && !self.shader_attached(shader) {
            self.shaders.remove(&shader);
        }
        detached
    }
}
