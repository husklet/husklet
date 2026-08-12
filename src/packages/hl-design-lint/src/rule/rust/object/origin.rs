use std::collections::BTreeMap;

use syn::{GenericArgument, ItemStruct, PathArguments, Type};

pub(super) fn field_origins(item: &ItemStruct) -> BTreeMap<String, String> {
    let syn::Fields::Named(fields) = &item.fields else {
        return BTreeMap::new();
    };
    fields
        .named
        .iter()
        .filter_map(|field| {
            let name = field.ident.as_ref()?.to_string();
            capability(&field.ty).map(|origin| (name, origin))
        })
        .collect()
}

fn capability(ty: &Type) -> Option<String> {
    match ty {
        Type::TraitObject(object) => object.bounds.iter().find_map(|bound| {
            let syn::TypeParamBound::Trait(bound) = bound else {
                return None;
            };
            qualified(&bound.path)
        }),
        Type::Path(path) => {
            let segment = path.path.segments.last()?;
            let name = segment.ident.to_string();
            if wrappers().contains(&name.as_str()) {
                return nested_type(segment).and_then(capability);
            }
            if collections().contains(&name.as_str()) || primitive(&name) {
                return None;
            }
            qualified(&path.path)
        }
        Type::Reference(reference) => capability(&reference.elem),
        _ => None,
    }
}

fn nested_type(segment: &syn::PathSegment) -> Option<&Type> {
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    arguments.args.iter().find_map(|argument| {
        let GenericArgument::Type(ty) = argument else {
            return None;
        };
        Some(ty)
    })
}

fn qualified(path: &syn::Path) -> Option<String> {
    let semantic = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .filter(|segment| !matches!(segment.as_str(), "crate" | "self" | "super"))
        .collect::<Vec<_>>();
    (semantic.len() >= 2).then(|| semantic[0].clone())
}

fn wrappers() -> &'static [&'static str] {
    &["Arc", "Box", "Mutex", "Option", "Rc", "RefCell", "RwLock", "Weak"]
}

fn collections() -> &'static [&'static str] {
    &[
        "BTreeMap",
        "BTreeSet",
        "BinaryHeap",
        "HashMap",
        "HashSet",
        "LinkedList",
        "Vec",
        "VecDeque",
    ]
}

fn primitive(name: &str) -> bool {
    matches!(
        name,
        "String"
            | "Path"
            | "PathBuf"
            | "OsStr"
            | "OsString"
            | "bool"
            | "char"
            | "f32"
            | "f64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
    )
}
