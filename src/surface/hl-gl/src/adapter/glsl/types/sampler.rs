use super::{Lexical, Type, Types};

pub(crate) struct StructSamplers;

#[derive(Clone)]
struct Leaf {
    path: String,
    ty: String,
}

#[derive(Clone)]
struct Parameter {
    structure: String,
    function: String,
    name: String,
    elements: usize,
    leaves: Vec<Leaf>,
    start: usize,
    end: usize,
}

impl StructSamplers {
    /// Split sampler-only structure parameters into ordinary sampler parameters. Mixed structures used
    /// directly are handled by data/sampler leaf reflection; the CTS helper cases carry no data leaves, so
    /// no synthetic value parameter is needed. Fixed arrays expand element-by-element, preserving the GL
    /// reflection names used by locations and sampler-unit setters.
    pub(crate) fn lower(source: &mut String) {
        let types = Types::parse(source);
        let tokens = Lexical::tokenize(source);
        let mut parameters = Vec::new();
        for (at, token) in tokens.iter().enumerate() {
            if !Lexical::function_parameter(&tokens, at) {
                continue;
            }
            let Some(leaves) = Self::sampler_leaves(&types, &Type::named(&token.text), "") else {
                continue;
            };
            let Some(name) = tokens.get(at + 1).filter(|token| token.identifier()) else {
                continue;
            };
            let Some(open) = (0..at).rev().find(|index| tokens[*index].text == "(") else {
                continue;
            };
            let Some(function) = open.checked_sub(1).and_then(|index| tokens.get(index)) else {
                continue;
            };
            let (elements, end) = if tokens.get(at + 2).is_some_and(|token| token.text == "[") {
                let Some(size) = tokens.get(at + 3).and_then(|token| token.text.parse().ok())
                else {
                    continue;
                };
                let Some(close) = tokens.get(at + 4).filter(|token| token.text == "]") else {
                    continue;
                };
                (size, close.end)
            } else {
                (1, name.end)
            };
            parameters.push(Parameter {
                structure: token.text.clone(),
                function: function.text.clone(),
                name: name.text.clone(),
                elements,
                leaves,
                start: token.start,
                end,
            });
        }

        for parameter in parameters.iter().rev() {
            let declaration = (0..parameter.elements)
                .flat_map(|element| {
                    parameter.leaves.iter().flat_map(move |leaf| {
                        let name = Self::parameter_name(parameter, element, &leaf.path);
                        let (texture, sampler, _) = Self::split(&leaf.ty);
                        [
                            format!("{texture} {name}_hltex"),
                            format!("{sampler} {name}_hlsmp"),
                        ]
                    })
                })
                .collect::<Vec<_>>()
                .join(", ");
            source.replace_range(parameter.start..parameter.end, &declaration);
        }

        for parameter in &parameters {
            for element in 0..parameter.elements {
                for leaf in &parameter.leaves {
                    let member = if parameter.elements == 1 {
                        format!("{}.{}", parameter.name, leaf.path)
                    } else {
                        format!("{}[{element}].{}", parameter.name, leaf.path)
                    };
                    let name = Self::parameter_name(parameter, element, &leaf.path);
                    let (_, _, constructor) = Self::split(&leaf.ty);
                    super::super::wreplace(
                        source,
                        &member,
                        &format!("{constructor}({name}_hltex, {name}_hlsmp)"),
                    );
                }
            }
            for uniform in Self::uniforms_of(source, &parameter.structure, parameter.elements) {
                let call = format!("{}({uniform})", parameter.function);
                let mut arguments = Vec::new();
                for element in 0..parameter.elements {
                    for leaf in &parameter.leaves {
                        let path = if parameter.elements == 1 {
                            format!("{uniform}.{}", leaf.path)
                        } else {
                            format!("{uniform}[{element}].{}", leaf.path)
                        };
                        let binding = Self::binding_name(&path);
                        arguments.push(format!("{binding}_hltex"));
                        arguments.push(format!("{binding}_hlsmp"));
                    }
                }
                let arguments = arguments.join(", ");
                super::super::wreplace(
                    source,
                    &call,
                    &format!("{}({arguments})", parameter.function),
                );
            }
        }
    }

    fn sampler_leaves(types: &Types, ty: &Type, prefix: &str) -> Option<Vec<Leaf>> {
        let structure = types.structure(ty.name())?;
        let mut out = Vec::new();
        for (name, field) in structure.fields() {
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}.{name}")
            };
            let dimensions = field.array_dimensions();
            let elements = dimensions.first().copied().flatten().unwrap_or(1);
            for element in 0..elements {
                let element_path = if dimensions.is_empty() {
                    path.clone()
                } else {
                    format!("{path}[{element}]")
                };
                if field.name().starts_with("sampler") {
                    out.push(Leaf {
                        path: element_path,
                        ty: field.name().to_owned(),
                    });
                } else {
                    let nested = Self::sampler_leaves(types, field, &element_path)?;
                    out.extend(nested);
                }
            }
        }
        (!out.is_empty()).then_some(out)
    }

    fn parameter_name(parameter: &Parameter, element: usize, path: &str) -> String {
        let path = path
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        if parameter.elements == 1 {
            format!("{}_{}", parameter.name, path)
        } else {
            format!("{}_{}_{}", parameter.name, element, path)
        }
    }

    fn binding_name(path: &str) -> String {
        path.chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn split(ty: &str) -> (&'static str, &'static str, &'static str) {
        match ty {
            "samplerCube" => ("textureCube", "sampler", "samplerCube"),
            "sampler2DShadow" => ("texture2D", "samplerShadow", "sampler2DShadow"),
            _ => ("texture2D", "sampler", "sampler2D"),
        }
    }

    fn uniforms_of(source: &str, structure: &str, elements: usize) -> Vec<String> {
        let tokens = Lexical::tokenize(source);
        let mut out = Vec::new();
        for window in tokens.windows(3) {
            if window[0].text != "uniform" || window[1].text != structure {
                continue;
            }
            let declared_elements = tokens
                .iter()
                .position(|token| token.start == window[2].start)
                .and_then(|at| tokens.get(at + 2))
                .and_then(|token| token.text.parse::<usize>().ok())
                .unwrap_or(1);
            if declared_elements == elements {
                out.push(window[2].text.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::StructSamplers;

    #[test]
    fn sampler_struct_parameter_and_fixed_array_call_split_into_binding_pairs() {
        let mut source = "struct S { sampler2D source; }; vec4 fun(S value[2]) { return texture2D(value[0].source, vec2(0.5)); } uniform S images[2]; void main(){ vec4 c=fun(images); }".to_owned();
        StructSamplers::lower(&mut source);
        assert!(source.contains(
            "fun(texture2D value_0_source_hltex, sampler value_0_source_hlsmp, texture2D value_1_source_hltex, sampler value_1_source_hlsmp)"
        ), "{source}");
        assert!(
            source.contains("sampler2D(value_0_source_hltex, value_0_source_hlsmp)"),
            "{source}"
        );
        assert!(source.contains("fun(images_0__source_hltex, images_0__source_hlsmp, images_1__source_hltex, images_1__source_hlsmp)"), "{source}");
    }

    #[test]
    fn mixed_and_data_only_parameters_are_not_partially_lowered() {
        for original in [
            "struct S { float value; sampler2D source; }; vec4 fun(S value){ return vec4(value.value); }",
            "struct S { float value; }; vec4 fun(S value){ return vec4(value.value); }",
        ] {
            let mut source = original.to_owned();
            StructSamplers::lower(&mut source);
            assert_eq!(source, original);
        }
    }
}
