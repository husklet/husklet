use super::*;

fn at(source: &str, needle: &str) -> usize {
    source.find(needle).unwrap() + needle.len()
}

#[test]
fn records_struct_schemas_including_arrays_and_nested_members() {
    let types =
        Types::parse("struct Leaf { vec3 color; }; struct Node { Leaf leaves[2]; int id; };");
    let node = types.structure("Node").unwrap();
    assert_eq!(node.field("id"), Some(&Type::named("int")));
    assert_eq!(
        node.field("leaves"),
        Some(&Type::named("Leaf").arrays(vec![Some(2)]))
    );
    assert_eq!(
        types.structure("Leaf").unwrap().field("color"),
        Some(&Type::named("vec3"))
    );
    assert_eq!(
        node.fields().map(|(name, _)| name).collect::<Vec<_>>(),
        vec!["id", "leaves"]
    );
}

#[test]
fn records_function_results_and_parameter_types_for_overloads() {
    let types = Types::parse(
        "struct S { float x; }; S make(S a, int n); S make(S a, float n) { return a; }",
    );
    let functions = types.functions("make");
    assert_eq!(functions.len(), 2);
    assert_eq!(functions[0].result(), &Type::named("S"));
    assert_eq!(
        functions[0].parameters(),
        &[Type::named("S"), Type::named("int")]
    );
    assert_eq!(
        functions[1].parameters(),
        &[Type::named("S"), Type::named("float")]
    );
}

#[test]
fn nested_blocks_select_the_nearest_visible_shadow() {
    let source = "struct S { float x; }; S value; void main(){ int value; { S value; value.x; } value; } value;";
    let types = Types::parse(source);
    assert_eq!(
        types.expression(at(source, "S value; value"), "value.x"),
        Some(Type::named("float"))
    );
    assert_eq!(
        types.expression(at(source, "} value"), "value"),
        Some(Type::named("int"))
    );
    assert_eq!(
        types.expression(source.len(), "value"),
        Some(Type::named("S"))
    );
}

#[test]
fn parameters_are_visible_in_the_body_and_can_be_shadowed_in_a_child_block() {
    let source = "struct S { int id; }; S pick(S item) { item.id; { int item; item; } item.id; }";
    let types = Types::parse(source);
    assert_eq!(
        types.expression(at(source, "{ item.id"), "item.id"),
        Some(Type::named("int"))
    );
    assert_eq!(
        types.expression(at(source, "int item; item"), "item"),
        Some(Type::named("int"))
    );
    assert_eq!(
        types.expression(at(source, "} item.id"), "item.id"),
        Some(Type::named("int"))
    );
}

#[test]
fn resolves_constructor_call_member_and_index_chains() {
    let source = "struct Leaf { vec4 color; }; struct Node { Leaf leaves[3]; }; Node make(); Node node; void main(){}";
    let types = Types::parse(source);
    let pos = source.len();
    assert_eq!(
        types.expression(pos, "Node(Leaf(vec4(1.0))).leaves[0].color.xyz"),
        Some(Type::named("vec3"))
    );
    assert_eq!(
        types.expression(pos, "make().leaves[2].color[1]"),
        Some(Type::named("float"))
    );
    assert_eq!(
        types.expression(pos, "node.leaves[1]"),
        Some(Type::named("Leaf"))
    );
}

#[test]
fn rejects_unknown_values_members_and_invalid_indexing() {
    let source = "struct S { float x; }; S value;";
    let types = Types::parse(source);
    assert_eq!(types.expression(source.len(), "missing"), None);
    assert_eq!(types.expression(source.len(), "value.missing"), None);
    assert_eq!(types.expression(source.len(), "value[0]"), None);
}

#[test]
fn declaration_visibility_starts_at_the_declaration_and_ends_with_its_scope() {
    let source = "struct S { float x; }; S global; S make(S parameter) { global.x; S first, matrix[2][3]; first.x; matrix[0][1].x; return S(1.0); } void other() { global.x; }";
    let types = Types::parse(source);
    assert_eq!(
        types.expression(at(source, "{ global.x"), "parameter.x"),
        Some(Type::named("float"))
    );
    assert_eq!(
        types.expression(at(source, "S first, matrix[2][3]; first"), "first.x"),
        Some(Type::named("float"))
    );
    assert_eq!(
        types.expression(at(source, "matrix[2][3]; first"), "matrix[0][1].x"),
        Some(Type::named("float"))
    );
    let other = at(source, "void other() { global");
    assert_eq!(types.expression(other, "parameter"), None);
    assert_eq!(types.expression(other, "first"), None);
    assert_eq!(types.expression(other, "S"), None);
}

#[test]
fn expression_results_cover_scalar_vector_and_matrix_indexing_and_swizzles() {
    let source = "vec4 color; ivec3 indices; mat3 basis;";
    let types = Types::parse(source);
    assert_eq!(
        types.expression(source.len(), "color.rgba"),
        Some(Type::named("vec4"))
    );
    assert_eq!(
        types.expression(source.len(), "indices[1]"),
        Some(Type::named("int"))
    );
    assert_eq!(
        types.expression(source.len(), "basis[2].xy"),
        Some(Type::named("vec2"))
    );
    assert_eq!(types.expression(source.len(), "color.unknown"), None);
}
