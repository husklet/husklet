//! Deterministic cross-language schema derived from authoritative Rust syntax.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use syn::{Attribute, Fields, GenericArgument, Item, PathArguments, Type};

use crate::{Capability, Frame, Kind, PROTOCOL, Topic};

const SOURCES: &[&str] = &[
    include_str!("request.rs"),
    include_str!("port.rs"),
    include_str!("manifest.rs"),
    include_str!("subscription.rs"),
    include_str!("capability.rs"),
];

enum Declaration {
    Struct(syn::ItemStruct),
    Enum(syn::ItemEnum),
    Alias(syn::ItemType),
}

/// Canonical pretty-printed protocol specification, terminated by one newline.
/// Panics rather than emitting a partial document when syntax is unsupported or
/// a named local wire type cannot be resolved.
#[must_use]
pub fn document() -> String {
    let declarations = declarations();
    assert_enum_all(
        declarations.get("Capability").unwrap(),
        Capability::ALL.iter().map(|value| value.as_str().to_owned()).collect(),
        "Capability::ALL",
    );
    assert_enum_all(
        declarations.get("Topic").unwrap(),
        Topic::ALL
            .iter()
            .map(|value| serde_json::to_value(value).unwrap().as_str().unwrap().to_owned())
            .collect(),
        "Topic::ALL",
    );
    let mut references = BTreeSet::new();
    let roots = ["Request", "Reply", "Failure"]
        .into_iter()
        .map(|name| {
            (
                name.to_ascii_lowercase(),
                schema(declarations.get(name).unwrap(), name, &mut references),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let mut definitions = serde_json::Map::new();
    let mut resolved = BTreeSet::new();
    let mut pending: Vec<_> = references.into_iter().collect();
    while let Some(name) = pending.pop() {
        if !resolved.insert(name.clone()) {
            continue;
        }
        if let Some(item) = declarations.get(&name) {
            let mut nested = BTreeSet::new();
            definitions.insert(name.clone(), schema(item, &name, &mut nested));
            pending.extend(nested);
        } else if let Some(value) = external(&name) {
            definitions.insert(name, value);
        } else {
            panic!("unresolved public wire type {name}");
        }
    }
    let kinds = Kind::ALL
        .iter()
        .map(|kind| {
            let bytes = Frame::new(crate::ChannelId::CONTROL, *kind, Vec::new())
                .encode()
                .unwrap();
            json!({"name":format!("{kind:?}").to_ascii_lowercase(),"code":bytes[8]})
        })
        .collect::<Vec<_>>();
    let value = json!({
        "specification_version":1, "protocol_version":PROTOCOL,
        "encoding":{"payload":"json","header_bytes":Frame::HEADER,"payload_limit_bytes":Frame::PAYLOAD_LIMIT,
            "header":["payload_length:u32le","channel:u32le","kind:u8","flags:u8","reserved:u16le"],
            "control_channel":0,"call_channel":crate::codec::CALLS.raw(),"host_channels":"even","extension_channels":"odd",
            "flags":{"end":1,"error":2,"coalesced":4},"kinds":kinds},
        "capabilities":Capability::ALL.iter().map(|c|json!({"wire":c.as_str(),"mutates":c.mutates(),"executes":c.executes()})).collect::<Vec<_>>(),
        "topics":Topic::ALL.iter().map(|t|json!({"wire":serde_json::to_value(t).unwrap(),"capability":t.capability().as_str()})).collect::<Vec<_>>(),
        "bounds":{
            "semantic_nodes":crate::port::SEMANTIC_NODE_LIMIT,"semantic_depth":crate::port::SEMANTIC_DEPTH_LIMIT,
            "semantic_text_bytes":crate::port::SEMANTIC_TEXT_LIMIT,"semantic_action_value_bytes":crate::port::SEMANTIC_ACTION_VALUE_LIMIT,
            "pane_input_bytes":crate::port::PANE_INPUT_BYTES,"terminal_command_argument_bytes":crate::port::TERMINAL_COMMAND_ARGUMENT_BYTES,
            "terminal_command_bytes":crate::port::TERMINAL_COMMAND_BYTES,"pane_inventory_items":crate::port::PANE_INVENTORY_LIMIT,
            "pane_text_bytes":crate::port::PANE_TEXT_BYTES,"extension_reference_bytes":crate::port::EXTENSION_REFERENCE_BYTES,
            "extension_job_bytes":crate::port::EXTENSION_JOB_BYTES
        },
        "roots":roots,"definitions":definitions
    });
    format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
}

fn assert_enum_all(item: &Declaration, runtime: Vec<String>, label: &str) {
    let Declaration::Enum(item) = item else {
        panic!("{label} does not describe an enum")
    };
    let rename_all = serde_value(&item.attrs, "rename_all");
    let source = item
        .variants
        .iter()
        .map(|variant| {
            serde_value(&variant.attrs, "rename")
                .unwrap_or_else(|| rename(&variant.ident.to_string(), rename_all.as_deref()))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        source, runtime,
        "{label} must exhaust the authoritative enum in declaration order"
    );
}

fn declarations() -> BTreeMap<String, Declaration> {
    let mut result = BTreeMap::new();
    for source in SOURCES {
        let file = syn::parse_file(source).expect("authoritative Rust source parses");
        for item in file.items {
            match item {
                Item::Struct(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                    result.insert(item.ident.to_string(), Declaration::Struct(item));
                }
                Item::Enum(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                    result.insert(item.ident.to_string(), Declaration::Enum(item));
                }
                Item::Type(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                    result.insert(item.ident.to_string(), Declaration::Alias(item));
                }
                _ => {}
            }
        }
    }
    result
}

fn schema(item: &Declaration, owner: &str, references: &mut BTreeSet<String>) -> Value {
    match item {
        Declaration::Alias(item) => type_schema(&item.ty, owner, references),
        Declaration::Struct(item) => {
            json!({"kind":"struct","fields":fields(&item.fields, owner, references),"serde":serde_meta(&item.attrs)})
        }
        Declaration::Enum(item) => {
            let rename_all = serde_value(&item.attrs, "rename_all");
            json!({"kind":"enum","serde":serde_meta(&item.attrs),"variants":item.variants.iter().map(|variant| {
                let name = serde_value(&variant.attrs,"rename").unwrap_or_else(||rename(&variant.ident.to_string(),rename_all.as_deref()));
                json!({"name":name,"payload":match &variant.fields { Fields::Unit=>json!({"kind":"unit"}), payload_fields=>json!({"kind":field_kind(payload_fields),"fields":fields(payload_fields,owner,references)}) }})
            }).collect::<Vec<_>>()})
        }
    }
}

fn fields(fields: &Fields, owner: &str, references: &mut BTreeSet<String>) -> Vec<Value> {
    fields.iter().enumerate().map(|(index, field)| {
        let name = field.ident.as_ref().map_or_else(||index.to_string(),ToString::to_string);
        let name = serde_value(&field.attrs,"rename").unwrap_or(name);
        json!({"name":name,"optional":serde_flag(&field.attrs,"default")||is_option(&field.ty),"schema":type_schema(&field.ty,owner,references)})
    }).collect()
}
fn field_kind(fields: &Fields) -> &'static str {
    match fields {
        Fields::Named(_) => "struct",
        Fields::Unnamed(_) => "tuple",
        Fields::Unit => "unit",
    }
}

fn type_schema(ty: &Type, owner: &str, references: &mut BTreeSet<String>) -> Value {
    match ty {
        Type::Reference(reference) => type_schema(&reference.elem, owner, references),
        Type::Tuple(tuple) => {
            json!({"kind":"tuple","items":tuple.elems.iter().map(|ty|type_schema(ty,owner,references)).collect::<Vec<_>>() })
        }
        Type::Array(array) => {
            json!({"kind":"array","length":integer_literal(&array.len),"of":type_schema(&array.elem,owner,references)})
        }
        Type::Path(path) => {
            let segment = path.path.segments.last().unwrap();
            let name = segment.ident.to_string();
            if name == "Self" {
                references.insert(owner.to_owned());
                return json!({"kind":"ref","name":owner});
            }
            let arguments = match &segment.arguments {
                PathArguments::AngleBracketed(args) => args
                    .args
                    .iter()
                    .filter_map(|arg| {
                        if let GenericArgument::Type(ty) = arg {
                            Some(ty)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>(),
                PathArguments::None => Vec::new(),
                other => panic!("unsupported path arguments {other:?}"),
            };
            match name.as_str() {
                "Option" => json!({"kind":"optional","of":type_schema(arguments[0],owner,references)}),
                "Vec" => json!({"kind":"array","of":type_schema(arguments[0],owner,references)}),
                "Box" => type_schema(arguments[0], owner, references),
                "BTreeMap" => {
                    json!({"kind":"map","key":type_schema(arguments[0],owner,references),"value":type_schema(arguments[1],owner,references)})
                }
                "String" => json!({"kind":"string"}),
                "bool" => json!({"kind":"boolean"}),
                "f32" => json!({"kind":"float","bits":32}),
                "f64" => json!({"kind":"float","bits":64}),
                primitive if integer(primitive).is_some() => {
                    let (bits, signed) = integer(primitive).unwrap();
                    json!({"kind":"integer","bits":bits,"signed":signed})
                }
                _ => {
                    references.insert(name.clone());
                    json!({"kind":"ref","name":name})
                }
            }
        }
        other => panic!("unsupported public wire type {other:?}"),
    }
}

fn serde_meta(attrs: &[Attribute]) -> Value {
    let mut map = serde_json::Map::new();
    for key in ["tag", "content", "rename_all"] {
        if let Some(value) = serde_value(attrs, key) {
            map.insert(key.into(), Value::String(value));
        }
    }
    for key in ["deny_unknown_fields", "transparent", "untagged"] {
        if serde_flag(attrs, key) {
            map.insert(key.into(), Value::Bool(true));
        }
    }
    Value::Object(map)
}
fn serde_value(attrs: &[Attribute], key: &str) -> Option<String> {
    let mut found = None;
    for attr in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(key) {
                found = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.input.peek(syn::Token![=]) {
                let _: syn::Expr = meta.value()?.parse()?;
            }
            Ok(())
        })
        .expect("supported serde attribute");
    }
    found
}
fn serde_flag(attrs: &[Attribute], key: &str) -> bool {
    let mut found = false;
    for attr in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(key) {
                found = true;
            }
            if meta.input.peek(syn::Token![=]) {
                let _: syn::Expr = meta.value()?.parse()?;
            }
            Ok(())
        })
        .expect("supported serde attribute");
    }
    found
}
fn is_option(ty: &Type) -> bool {
    matches!(ty,Type::Path(path) if path.path.segments.last().is_some_and(|s|s.ident=="Option"))
}
fn rename(name: &str, case: Option<&str>) -> String {
    let separator = match case {
        Some("snake_case") => '_',
        Some("kebab-case") => '-',
        None => return name.to_owned(),
        Some(other) => panic!("unsupported serde rename_all {other}"),
    };
    name.chars()
        .enumerate()
        .flat_map(|(i, c)| {
            if c.is_ascii_uppercase() && i > 0 {
                vec![separator, c.to_ascii_lowercase()]
            } else {
                vec![c.to_ascii_lowercase()]
            }
        })
        .collect()
}
fn integer(ty: &str) -> Option<(u8, bool)> {
    Some(match ty {
        "u8" => (8, false),
        "u16" => (16, false),
        "u32" => (32, false),
        "u64" | "usize" => (64, false),
        "i8" => (8, true),
        "i16" => (16, true),
        "i32" => (32, true),
        "i64" | "isize" => (64, true),
        _ => return None,
    })
}
fn integer_literal(expr: &syn::Expr) -> u64 {
    if let syn::Expr::Lit(value) = expr {
        if let syn::Lit::Int(value) = &value.lit {
            return value.base10_parse().unwrap();
        }
    }
    panic!("unsupported array length")
}
fn external(name: &str) -> Option<Value> {
    Some(match name {
        "RelativePath" | "ExtensionName" | "PeerName" => json!({"kind":"string"}),
        "Grant" => json!({"kind":"array","of":{"kind":"ref","name":"Capability"},"unique":true}),
        "Frame" => json!({"kind":"external_ref","package":"hl-gui","name":"Frame"}),
        "SourceMutation" => json!({"kind":"external_ref","package":"hl-gui","name":"SourceMutation"}),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn checked_in_spec_is_current() {
        assert_eq!(super::document(), include_str!("../protocol/v1.json"));
    }
}
