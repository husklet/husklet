//! Shader and program object collections.

use super::{Program, Shader};
use std::collections::HashMap;

/// The per-context shader + program tables: GL name → object, each with a monotonic name counter. GL
/// shares one name space per object kind; name `0` is the reserved sentinel.
#[derive(Debug, Default)]
pub struct Programs {
    shaders: HashMap<u32, Shader>,
    programs: HashMap<u32, Program>,
    next_shader: u32,
    next_program: u32,
}

impl Programs {
    pub fn new() -> Self {
        Self {
            shaders: HashMap::new(),
            programs: HashMap::new(),
            next_shader: 1,
            next_program: 1,
        }
    }

    /// `glCreateShader(kind)` — mint a shader name.
    pub fn create_shader(&mut self, kind: u32) -> u32 {
        let name = self.next_shader;
        self.next_shader += 1;
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
        }
    }

    pub fn shader(&self, name: u32) -> Option<&Shader> {
        self.shaders.get(&name)
    }

    /// `glCreateProgram()` — mint a program name.
    pub fn create(&mut self) -> u32 {
        let name = self.next_program;
        self.next_program += 1;
        self.programs.insert(name, Program::default());
        name
    }

    /// `glAttachShader(program, shader)` — record the attachment by the shader's kind.
    pub fn attach(&mut self, program: u32, shader: u32) {
        let kind = self.shaders.get(&shader).map(|s| s.kind).unwrap_or(0);
        if let Some(p) = self.programs.get_mut(&program) {
            match kind {
                crate::model::glconst::GL_VERTEX_SHADER => p.vs = shader,
                crate::model::glconst::GL_FRAGMENT_SHADER => p.fs = shader,
                crate::model::glconst::GL_COMPUTE_SHADER => p.cs = shader,
                _ => {}
            }
        }
    }

    /// `glLinkProgram(program)` — translate + reflect the attached shaders. Returns `false` if the
    /// program name or either attached shader is unknown.
    pub fn link(&mut self, program: u32) -> bool {
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

    /// `glDeleteProgram(name)` — drop the program object (deleting `0` is a silent no-op; GL defines it
    /// so). This model has no deferred-delete-while-current subtlety: the object is removed immediately.
    /// Returns `false` for an unknown / zero name.
    pub fn delete(&mut self, name: u32) -> bool {
        if name == 0 {
            return false;
        }
        self.programs.remove(&name).is_some()
    }

    /// `glDeleteShader(name)` — drop the shader object (deleting `0` is a silent no-op). Attachments hold
    /// only the shader NAME (captured at link), so a delete does not disturb a linked program's reflected
    /// IR. Returns `false` for an unknown / zero name.
    pub fn delete_shader(&mut self, name: u32) -> bool {
        if name == 0 {
            return false;
        }
        self.shaders.remove(&name).is_some()
    }

    /// `glDetachShader(program, shader)` — clear the matching attachment slot. Returns `false` if the
    /// program is unknown or `shader` is not attached (the caller raises the spec error).
    pub fn detach(&mut self, program: u32, shader: u32) -> bool {
        match self.programs.get_mut(&program) {
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
        }
    }
}
