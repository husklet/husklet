//! Program reflection queries.

use super::Program;
use hl_gpu::protocol::model::kernel::GlslDescriptor;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UniformLocation {
    Data { declaration: usize, element: usize },
    Sampler { element: usize },
}

impl Program {
    /// Host shader locations fed by one public GL attribute location.
    pub fn host_attr_locations(&self, public: usize) -> Vec<u32> {
        let declarations = crate::adapter::glsl::Source::new(&self.vs_src).vertex_attrs();
        self.attrib_locations
            .iter()
            .filter_map(|(name, location)| {
                let declaration = declarations
                    .iter()
                    .find(|declaration| declaration.name == *name)?;
                let offset = public.checked_sub(*location as usize)?;
                (offset < declaration.location_span() as usize).then(|| {
                    self.attrib_host_locations
                        .get(name)
                        .copied()
                        .map(|host| host + offset as u32)
                })?
            })
            .collect()
    }

    /// Component count of the active vertex input at `location`.
    pub fn vertex_attr_components(&self, location: usize) -> Option<i32> {
        // A matrix or array attribute occupies one location PER COLUMN / PER ELEMENT, and each of those is
        // a separate vertex input the pipeline must supply. Matching `location` exactly left every column
        // after the first unknown, so a declared-but-disabled `mat2` got no constant attribute for its
        // second column — the shader declared a location the pipeline never supplied, pipeline creation
        // failed, and the draw wedged the context. Each location carries ONE COLUMN's components, so a
        // `mat4x2` supplies four locations of two components, not one of eight.
        let declarations = crate::adapter::glsl::Source::new(&self.vs_src).vertex_attrs();
        for (name, &base) in &self.attrib_locations {
            let Some(declaration) = declarations
                .iter()
                .find(|declaration| &declaration.name == name)
            else {
                continue;
            };
            let base = base as usize;
            let span = declaration.location_span() as usize;
            if (base..base.saturating_add(span)).contains(&location) {
                if let Some(components) = components_per_location(&declaration.ty) {
                    return Some(components);
                }
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
    let base = declaration[assignment..end].trim().parse::<usize>().ok()?;
    let tokens = declaration[end + 1..]
        .split_whitespace()
        .collect::<Vec<_>>();
    let input = tokens.iter().position(|token| *token == "in")?;
    let ty = tokens
        .iter()
        .skip(input + 1)
        .find(|token| !matches!(**token, "highp" | "mediump" | "lowp"))?;
    // The emitted declaration spans one location per column; `wanted` may name any of them.
    let span = matrix_shape(ty).map_or(1, |(columns, _)| columns as usize);
    if !(base..base.saturating_add(span)).contains(&wanted) {
        return None;
    }
    components_per_location(ty)
}

/// `(columns, rows)` for a matrix type spelling, or `None` when `ty` is not a matrix. GLSL's `matCxR` has
/// `C` columns of `R` rows, and the square `matN` is `matNxN`.
fn matrix_shape(ty: &str) -> Option<(i32, i32)> {
    let rest = ty.strip_prefix("mat")?;
    let digit = |byte: Option<&u8>| {
        byte.and_then(|byte| char::from(*byte).to_digit(10))
            .map(|value| value as i32)
            .filter(|value| (2..=4).contains(value))
    };
    let bytes = rest.as_bytes();
    let columns = digit(bytes.first())?;
    match bytes.get(1) {
        None => Some((columns, columns)),
        Some(b'x') => digit(bytes.get(2)).map(|rows| (columns, rows)),
        Some(_) => None,
    }
}

/// Components supplied at ONE location of a declaration. A matrix contributes one column per location, so
/// this is its ROW count — `mat4x2` is four locations of two components. Everything else contributes all
/// of its components at a single location, and an array repeats its element type per location.
fn components_per_location(ty: &str) -> Option<i32> {
    if let Some((_, rows)) = matrix_shape(ty) {
        return Some(rows);
    }
    components(ty)
}
