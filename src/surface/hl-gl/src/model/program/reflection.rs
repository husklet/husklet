//! Program reflection queries.

use super::Program;
use hl_gpu::protocol::model::kernel::GlslDescriptor;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UniformLocation {
    Data { declaration: usize, element: usize },
    Sampler { element: usize },
}

impl Program {
    /// Component count of the active vertex input at `location`.
    pub fn vertex_attr_components(&self, location: usize) -> Option<i32> {
        let name = self
            .attrib_locations
            .iter()
            .find_map(|(name, &bound)| (bound as usize == location).then_some(name));
        if let Some(declaration) = crate::adapter::glsl::Source::new(&self.vs_src)
            .vertex_attrs()
            .into_iter()
            .find(|declaration| Some(&declaration.name) == name)
        {
            if let Some(components) = components(&declaration.ty) {
                return Some(components);
            }
        }
        let descriptor = GlslDescriptor::from_words(self.vs_ir.as_ref()?)?.ok()?;
        descriptor
            .source
            .split(';')
            .find_map(|declaration| linked_input(declaration, location))
    }

    /// `glGetUniformLocation(name)` — resolve `name` to the location the `glUniform*` recording ops key
    /// on. Locations form one collision-free namespace: every data-array element first, followed by every
    /// sampler-array element. `name` and `name[0]` identify the same first element.
    pub fn uniform_location(&self, name: &str) -> i32 {
        if !self.linked {
            return -1;
        }
        let (base_name, wanted_element) = uniform_name(name);
        let mut location = 0usize;
        for uniform in &self.unis {
            let elements = uniform.arr.max(1) as usize;
            if uniform.name == base_name {
                let element = wanted_element.unwrap_or(0);
                return if element < elements {
                    i32::try_from(location + element).unwrap_or(-1)
                } else {
                    -1
                };
            }
            location += elements;
        }
        for (index, sampler) in self.samp_names.iter().enumerate() {
            let elements = self.samp_arrays.get(index).copied().unwrap_or(1).max(1) as usize;
            if sampler == base_name {
                let element = wanted_element.unwrap_or(0);
                return if element < elements {
                    i32::try_from(location + element).unwrap_or(-1)
                } else {
                    -1
                };
            }
            location += elements;
        }
        -1
    }

    pub fn location(&self, location: i32) -> Option<UniformLocation> {
        let mut location = usize::try_from(location).ok()?;
        for (declaration, uniform) in self.unis.iter().enumerate() {
            let elements = uniform.arr.max(1) as usize;
            if location < elements {
                return Some(UniformLocation::Data {
                    declaration,
                    element: location,
                });
            }
            location -= elements;
        }
        (location < self.samp_units.len()).then_some(UniformLocation::Sampler { element: location })
    }

    /// `glGetAttribLocation(name)` — the location assigned by the most recent successful link.
    pub fn attrib_location(&self, name: &str) -> i32 {
        if !self.linked {
            return -1;
        }
        self.attrib_locations
            .get(name)
            .and_then(|location| i32::try_from(*location).ok())
            .unwrap_or(-1)
    }
}

fn uniform_name(name: &str) -> (&str, Option<usize>) {
    let Some(open) = name.rfind('[') else {
        return (name, None);
    };
    let Some(index) = name.strip_suffix(']').and_then(|name| name.get(open + 1..)) else {
        return (name, None);
    };
    match index.parse::<usize>() {
        Ok(index) => (&name[..open], Some(index)),
        Err(_) => (name, None),
    }
}

fn components(ty: &str) -> Option<i32> {
    match ty {
        "float" | "int" | "uint" | "bool" => Some(1),
        "vec2" | "ivec2" | "uvec2" | "bvec2" => Some(2),
        "vec3" | "ivec3" | "uvec3" | "bvec3" => Some(3),
        "vec4" | "ivec4" | "uvec4" | "bvec4" => Some(4),
        _ => None,
    }
}

fn linked_input(declaration: &str, wanted: usize) -> Option<i32> {
    let location = declaration.find("location")?;
    let assignment = declaration[location..].find('=')? + location + 1;
    let end = declaration[assignment..].find(')')? + assignment;
    if declaration[assignment..end].trim().parse::<usize>().ok()? != wanted {
        return None;
    }
    let tokens = declaration[end + 1..]
        .split_whitespace()
        .collect::<Vec<_>>();
    let input = tokens.iter().position(|token| *token == "in")?;
    let ty = tokens
        .iter()
        .skip(input + 1)
        .find(|token| !matches!(**token, "highp" | "mediump" | "lowp"))?;
    components(ty)
}
