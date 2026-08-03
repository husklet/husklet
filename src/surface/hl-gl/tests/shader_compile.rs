use hl_gl::model::context::GlContext;
use hl_gl::model::glconst::{
    GL_COMPILE_STATUS, GL_FALSE, GL_FRAGMENT_SHADER, GL_TRUE, GL_VERTEX_SHADER,
};
use hl_gl::service::{query, record};

fn compile(context: &mut GlContext, kind: u32, source: &str) -> (i32, String) {
    let shader = record::create_shader(context, kind);
    record::shader_source(context, shader, source);
    record::compile_shader(context, shader);
    (
        query::get_shaderiv(context, shader, GL_COMPILE_STATUS),
        query::shader_info_log(context, shader).to_string(),
    )
}

#[test]
fn es2_rejects_the_complete_invalid_implicit_arithmetic_matrix() {
    let floats = ["float", "vec2", "vec3", "vec4"];
    let integers = ["int", "ivec2", "ivec3", "ivec4"];
    let mut context = GlContext::new();

    for operation in ["+", "-", "*", "/"] {
        for float_type in floats {
            for integer_type in integers {
                for shader_kind in [GL_VERTEX_SHADER, GL_FRAGMENT_SHADER] {
                    for result_type in [float_type, integer_type] {
                        let source = format!(
                            "precision mediump float; precision mediump int; void main() {{ {float_type} a; {integer_type} b; {result_type} c = a {operation} b; }}"
                        );
                        let (status, log) = compile(&mut context, shader_kind, &source);
                        assert_eq!(
                            status, GL_FALSE as i32,
                            "accepted {result_type} = {float_type} {operation} {integer_type}"
                        );
                        assert!(
                            log.contains("implicit conversion"),
                            "missing diagnostic: {log:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn same_basic_kind_arithmetic_remains_accepted() {
    let mut context = GlContext::new();
    for source in [
        "void main(){ float a; float b; float c = a + b; }",
        "void main(){ int a; int b; int c = a / b; }",
        "void main(){ vec3 a; float b; vec3 c = a * b; }",
        "void main(){ ivec4 a; int b; ivec4 c = a - b; }",
    ] {
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
        assert_eq!(status, GL_TRUE as i32, "rejected positive control: {log}");
    }
}

#[test]
fn es2_rejects_integer_literal_for_float_function_parameter() {
    let mut context = GlContext::new();
    let source = "precision mediump float; precision mediump int; void func(float f){} void main(){func(2); gl_FragColor=vec4(1.0);}";
    let (status, log) = compile(&mut context, GL_FRAGMENT_SHADER, source);
    assert_eq!(status, GL_FALSE as i32);
    assert!(log.contains("parameter type 'float'"), "{log}");

    let (status, log) = compile(
        &mut context,
        GL_FRAGMENT_SHADER,
        "precision mediump float; void func(float f){} void main(){func(2.0);}",
    );
    assert_eq!(status, GL_TRUE as i32, "rejected matching call: {log}");
}

#[test]
fn es2_rejects_function_declaration_definition_and_call_contract_violations() {
    let invalid = [
        "void func(float f){} void main(){func(1.0,2.0);}",
        "void func(float f){} void main(){func();}",
        "void func(vec2 f){} void main(){func(2.0);}",
        "void func(vec3 f){} void main(){func(vec2(2.0));}",
        "void func(vec3 f); void func(vec3 f){} void func(vec3 f){} void main(){}",
        "void func(vec3 f); float func(vec3 f){return f.x;} void main(){}",
        "void func(vec3 f); void func(const vec3 f){} void main(){}",
        "void func(out vec3 f); void func(inout vec3 f){} void main(){}",
        "void func(vec3 f[]); void main(){}",
        "void func(vec3 f[3]); void func(vec3 f[3]){} void main(){vec3 values[4];func(values);}",
        "void main(){func(1.0);} void func(float f){}",
        "float func(float f){return;} void main(){}",
        "void func(){return 1.0;} void main(){}",
        "float func(float f){return f;} int func(float f){return int(f);} void main(){}",
        "lowp float func(float f){return f;} mediump float func(float f){return f;} void main(){}",
        "void main(float f){}",
        "float main(){}",
        "main(){}",
        "void main(){float nested(float f);}",
        "void main(){float nested(float f){return f;}}",
        "struct Foo { float value; float array[2]; }; Foo func(){Foo f; return f;} void main(){}",
        "struct Foo { float value; }; float Foo(float f){return f;} void main(){}",
        "void func(const float f){f=1.0;} void main(){}",
        "void func(const float f[3]){f[0]=1.0;} void main(){}",
        "int func(const int a){const int b=-a; return b;} void main(){}",
        "int func(const int a){int values[a]; return values[0];} void main(){}",
        "void func(uniform float f){} void main(){}",
        "uniform float func(float f){return f;} void main(){}",
        "void func(){break;} void main(){}",
        "void func(){continue;} void main(){}",
    ];
    for source in invalid {
        for kind in [GL_VERTEX_SHADER, GL_FRAGMENT_SHADER] {
            let mut context = GlContext::new();
            let (status, log) = compile(&mut context, kind, source);
            assert_eq!(
                status, GL_FALSE as i32,
                "accepted invalid function source:\n{source}"
            );
            assert!(!log.is_empty(), "missing diagnostic for:\n{source}");
        }
    }
}

#[test]
fn valid_function_overloads_prototypes_calls_and_const_reads_remain_accepted() {
    let valid = [
        "float func(float value); float func(float other){return other;} void main(){float x=func(1.0);}",
        "float func(float f){return f;} int func(int i){return i;} void main(){float x=func(1.0);}",
        "int func(const int a){return 2*a;} void main(){int x=func(3);}",
        "void func(){for(int i=0;i<1;i++){continue;break;}} void main(){}",
        "struct Pair{float x;}; float func(Pair pair){return pair.x;} void main(){}",
    ];
    for source in valid {
        let mut context = GlContext::new();
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
        assert_eq!(
            status, GL_TRUE as i32,
            "false function rejection: {log}\n{source}"
        );
    }
}

#[test]
fn es2_enforces_parameter_qualifier_grammar_order() {
    let invalid = [
        "lowp const in float value",
        "const lowp in float value",
        "in const lowp float value",
        "lowp in const float value",
        "in lowp const float value",
        "lowp const float value",
        "lowp in float value",
    ];
    for parameter in invalid {
        let source = format!("void func({parameter}){{}} void main(){{}}");
        let mut context = GlContext::new();
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, &source);
        assert_eq!(
            status, GL_FALSE as i32,
            "accepted qualifier order: {parameter}"
        );
        assert!(log.contains("out of order"), "bad diagnostic: {log}");
    }

    for parameter in [
        "const in lowp float value",
        "out mediump float value",
        "inout mediump float value",
        "const lowp float value",
        "mediump float value",
        "in lowp float value",
    ] {
        let source = format!("void func({parameter}){{}} void main(){{}}");
        let mut context = GlContext::new();
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, &source);
        assert_eq!(
            status, GL_TRUE as i32,
            "rejected valid qualifier order: {log}"
        );
    }
}

#[test]
fn es2_vector_constructors_reject_an_undersized_component_count() {
    let mut context = GlContext::new();
    for source_type in ["vec2", "ivec2", "bvec2"] {
        for destination_type in ["vec3", "ivec3", "bvec3", "vec4", "ivec4", "bvec4"] {
            let source = format!(
                "void main(){{ {source_type} source_value; {destination_type} result={destination_type}(source_value); }}"
            );
            let (status, log) = compile(&mut context, GL_VERTEX_SHADER, &source);
            assert_eq!(
                status, GL_FALSE as i32,
                "accepted undersized constructor: {source}"
            );
            assert!(
                log.contains("components"),
                "missing component diagnostic: {log}"
            );
        }
    }
    for source_type in ["vec3", "ivec3", "bvec3"] {
        for destination_type in ["vec4", "ivec4", "bvec4"] {
            let source = format!(
                "void main(){{ {source_type} source_value; {destination_type} result={destination_type}(source_value); }}"
            );
            let (status, _) = compile(&mut context, GL_FRAGMENT_SHADER, &source);
            assert_eq!(
                status, GL_FALSE as i32,
                "accepted undersized constructor: {source}"
            );
        }
    }
    for source in [
        "void main(){vec3 value=vec3(1.0,2.0,3.0,4.0);}",
        "void main(){vec3 source_value; vec3 value=vec3(source_value,1.0);}",
        "void main(){vec2 left; vec2 right; vec3 value=vec3(left,right,1.0);}",
    ] {
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
        assert_eq!(status, GL_FALSE as i32, "accepted unused argument: {log}");
    }
}

#[test]
fn legal_vector_splats_conversions_and_component_composition_remain_accepted() {
    let mut context = GlContext::new();
    for source in [
        "void main(){vec4 value=vec4(1.0);}",
        "void main(){ivec3 source_value; vec3 value=vec3(source_value);}",
        "void main(){vec2 source_value; vec3 value=vec3(source_value,1.0);}",
        "void main(){vec2 left; vec2 right; vec4 value=vec4(left,right);}",
        "void main(){vec4 source_value; vec4 value=vec4(source_value.xyz,1.0);}",
        "vec3 helper(){return vec3(1.0);} void main(){vec4 value=vec4(helper(),1.0);}",
        "void main(){vec2 left; vec3 right; vec4 value=vec4(left,right);}",
        "void main(){vec2 left; vec4 right; vec4 value=vec4(left,right);}",
        "void main(){vec4 value=vec4(0.0,vec3(1.0).r,2.0,3.0);}",
        "void main(){bvec4 value=bvec4(vec4(1.0).w*asin(0.0),true,false,true);}",
    ] {
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
        assert_eq!(
            status, GL_TRUE as i32,
            "false constructor rejection: {log}\n{source}"
        );
    }
}

#[test]
fn parenthesized_constructor_arguments_are_not_inferred_as_vector_declarations() {
    let source = "void main(){ const bvec4 h = bvec4(0, (abs(exp2(float(0.5)) * float(int(-1.0)) - (vec3(-1, -4.5, false).x - cos(1.125))) < 0.001) == true, 1.0, (ivec4(-16, -5, 12, -1.5).g) - -6 * 1); }";
    let mut context = GlContext::new();
    let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
    assert_eq!(status, GL_TRUE as i32, "{log}\n{source}");
}

#[test]
fn es2_rejects_invalid_lexical_scope_and_symbol_namespace_uses() {
    let invalid = [
        "int value; float value; void main(){}",
        "void main(){int value; float value;}",
        "float func(float x); float func(float x); float func(float x){return x;} void main(){}",
        "float sin(float x); void main(){}",
        "float sin(float x){return x;} void main(){}",
        "void item(int x); struct item{int x;}; void main(){}",
        "void item(int x); float item; void main(){}",
        "void func(){value=2.0;} float value; void main(){func();}",
        "void main(){float left=1.0; left=right; float right=2.0;}",
        "float func(float x){return Shape(x).value;} struct Shape{float value;}; void main(){}",
        "void main(){{float inner=1.0;} float result=inner;}",
        "void main(){if(true) float inner=1.0; float result=inner;}",
        "void main(){if(false) float left=1.0; else float right=2.0; float result=right;}",
        "void main(){float result; if(true){float inner=1.0;}else{result=inner;}}",
        "void main(){float value=value;}",
        "float func(float before); float func(float actual){return before;} void main(){}",
        "void main(){for(int index=0;index<2;index++){int index=1;}}",
        "void main(){for(int index=0;int condition=(index<2);index++){int condition=1;}}",
        "void main(){for(int index=0;int index=(index<2);index++){}}",
        "void main(){int count=0;while(bool active=(count<2)){bool active=false;count++;}}",
        "void main(){for(int index=0;index<2;index++){} int result=index;}",
        "void main(){int count=0;while(bool active=(count<2)){count++;} bool result=active;}",
    ];
    for source in invalid {
        let mut context = GlContext::new();
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
        assert_eq!(status, GL_FALSE as i32, "accepted invalid scope:\n{source}");
        assert!(!log.is_empty(), "missing scope diagnostic: {source}");
    }
}

#[test]
fn legal_nested_shadowing_and_symbol_lifetimes_remain_accepted() {
    let valid = [
        "float global_value; void func(){global_value=1.0;} void main(){func();}",
        "void main(){float value=1.0; {float value=2.0;} value=3.0;}",
        "void main(){if(true){float branch=1.0; branch=2.0;}else{float branch=3.0; branch=4.0;}}",
        "float func(float value){return value;} void main(){float value=func(1.0);}",
        "struct Pair{float value;}; float func(Pair pair){return pair.value;} void main(){}",
        "void main(){float first=1.0; float second=first;}",
        "int func(int func){return func;} void main(){int value=func(1);}",
        "int func(int input_value,int value){int value=5;return input_value+value;} void main(){}",
        "int outer_value; void main(){int outer_value=1;{int outer_value=outer_value+5;}}",
        "precision mediump float; precision mediump int; bool isOk(float a,int b){float atemp=a+0.5;return float(b)<=atemp&&atemp<=float(b+1);} varying float v_in0; uniform int ref_out0; int out0; void main(){int in0=int(v_in0); int a=in0; {int a=a+5,b=a-5;out0=b;a=42;} out0=out0+a-in0; bool result=isOk(float(out0),ref_out0); gl_FragColor=vec4(result);}",
        "void main(){float result=0.0; for(int index=0;index<2;index++){result+=float(index);}}",
        "void main(){int count=0; while(bool active=(count<2)){if(active){count++;}}}",
    ];
    for source in valid {
        let mut context = GlContext::new();
        let (status, log) = compile(&mut context, GL_FRAGMENT_SHADER, source);
        assert_eq!(
            status, GL_TRUE as i32,
            "false scope rejection: {log}\n{source}"
        );
    }
}

#[test]
fn es2_rejects_static_use_of_both_fragment_output_interfaces() {
    let mut context = GlContext::new();
    for body in [
        "gl_FragColor=vec4(1.0); gl_FragData[0]=vec4(1.0);",
        "if(false) gl_FragColor=vec4(1.0); else gl_FragData[0]=vec4(1.0);",
        "gl_FragColor=vec4(1.0); } void unused(){gl_FragData[0]=vec4(1.0);",
    ] {
        let source = format!("void main(){{{body}}}");
        let (status, log) = compile(&mut context, GL_FRAGMENT_SHADER, &source);
        assert_eq!(status, GL_FALSE as i32, "accepted mixed outputs: {source}");
        assert!(log.contains("gl_FragColor") && log.contains("gl_FragData"));
    }
    for body in ["gl_FragColor=vec4(1.0);", "gl_FragData[0]=vec4(1.0);"] {
        let (status, log) = compile(
            &mut context,
            GL_FRAGMENT_SHADER,
            &format!("void main(){{{body}}}"),
        );
        assert_eq!(status, GL_TRUE as i32, "rejected one output: {log}");
    }
}

#[test]
fn lexical_shapes_do_not_create_false_implicit_conversion_diagnostics() {
    let mut context = GlContext::new();
    let controls = [
        // Comments are not source tokens.
        "void main(){ float a; float b; /* int fake; a + fake; */ float c=a+b; // a + fake\n }",
        // Constructor and function-call syntax is not a variable declaration or a binary expression.
        "float twice(float x){ return x+x; } void main(){ int i; float f=twice(float(i)); }",
        // The inner integer names shadow the outer float names; both expressions are same-kind in scope.
        "void main(){ float a; float b; float c=a+b; { int a; int b; int c=a+b; } c=a+b; }",
        // Reusing a name after its inner declaration leaves scope must resolve to the outer declaration.
        "void main(){ float a; { int a; int b; int c=a+b; } float b; float c=a+b; }",
        // Compound and nested same-kind expressions remain legal.
        "void main(){ vec3 a; float b; vec3 c=(a*b)+(a/b); c += a; }",
    ];
    for source in controls {
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
        assert_eq!(status, GL_TRUE as i32, "false rejection: {log}\n{source}");
    }
}

#[test]
fn es2_rejects_keywords_reserved_words_and_reserved_identifier_forms() {
    let keywords = [
        "attribute",
        "const",
        "uniform",
        "varying",
        "break",
        "continue",
        "do",
        "for",
        "while",
        "if",
        "else",
        "in",
        "out",
        "inout",
        "float",
        "int",
        "void",
        "bool",
        "true",
        "false",
        "lowp",
        "mediump",
        "highp",
        "precision",
        "invariant",
        "discard",
        "return",
        "mat2",
        "mat3",
        "mat4",
        "vec2",
        "vec3",
        "vec4",
        "ivec2",
        "ivec3",
        "ivec4",
        "bvec2",
        "bvec3",
        "bvec4",
        "sampler2D",
        "samplerCube",
        "struct",
    ];
    let reserved = [
        "asm",
        "class",
        "union",
        "enum",
        "typedef",
        "template",
        "this",
        "packed",
        "goto",
        "switch",
        "default",
        "inline",
        "noinline",
        "volatile",
        "public",
        "static",
        "extern",
        "external",
        "interface",
        "flat",
        "long",
        "short",
        "double",
        "half",
        "fixed",
        "unsigned",
        "superp",
        "input",
        "output",
        "hvec2",
        "hvec3",
        "hvec4",
        "dvec2",
        "dvec3",
        "dvec4",
        "fvec2",
        "fvec3",
        "fvec4",
        "sampler1D",
        "sampler3D",
        "sampler1DShadow",
        "sampler2DShadow",
        "sampler2DRect",
        "sampler3DRect",
        "sampler2DRectShadow",
        "sizeof",
        "cast",
        "namespace",
        "using",
    ];
    let invalid = [
        "__invalid",
        "in__valid",
        "invalid__",
        "gl_Invalid",
        "0123",
        "0invalid",
    ];
    let mut context = GlContext::new();
    for identifier in keywords.into_iter().chain(reserved).chain(invalid) {
        for kind in [GL_VERTEX_SHADER, GL_FRAGMENT_SHADER] {
            let source = format!("void main(){{ float {identifier} = 1.0; }}");
            let (status, log) = compile(&mut context, kind, &source);
            assert_eq!(status, GL_FALSE as i32, "accepted identifier {identifier}");
            assert!(
                log.contains(identifier) && log.contains("identifier"),
                "bad log: {log:?}"
            );
        }
    }
}

#[test]
fn keyword_tokens_in_their_grammar_roles_remain_accepted() {
    let mut context = GlContext::new();
    let controls = [
        "attribute vec4 position; varying vec2 uv; void main(){ if(true){ gl_Position=position; } else { discard; } }",
        "precision mediump float; uniform sampler2D image; void main(){ for(int i=0;i<1;i++){ continue; } return; }",
        "struct Pair { float left; int right; }; void helper(in float a, out float b){ b=a; } void main(){ float x; helper(1.0,x); }",
        "void main(){ /* float class; */ float classic=1.0; float assembly=classic; }",
    ];
    for source in controls {
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
        assert_eq!(
            status, GL_TRUE as i32,
            "false keyword rejection: {log}\n{source}"
        );
    }
}

#[test]
fn es2_rejects_every_reserved_integer_operator() {
    let operators = [
        "%", "~", "<<", ">>", "&", "^", "|", "%=", "<<=", ">>=", "&=", "^=", "|=",
    ];
    let mut context = GlContext::new();
    for operator in operators {
        for kind in [GL_VERTEX_SHADER, GL_FRAGMENT_SHADER] {
            let statement = if operator == "~" {
                "value = ~value;".to_string()
            } else {
                format!("value {operator} 1;")
            };
            let source = format!("void main(){{ int value=100; {statement} }}");
            let (status, log) = compile(&mut context, kind, &source);
            assert_eq!(status, GL_FALSE as i32, "accepted ES2 operator {operator}");
            assert!(
                log.contains(operator) && log.contains("reserved operator"),
                "bad log: {log:?}"
            );
        }
    }
}

#[test]
fn legal_es2_operators_and_non_tokens_remain_accepted() {
    let mut context = GlContext::new();
    let controls = [
        "void main(){ int a=1; int b=2; bool c=a<b || a>=b; c=c && a<=b; }",
        "void main(){ int value=1; /* value %= 1; value << 2; */ value += 1; }",
        "#if (8 + 3 % 2) == 9\n#define VALUE 1\n#endif\nvoid main(){ int value=VALUE; }",
        "#version 300 es\nvoid main(){ int value=1; value%=1; value<<=2; value=~value; }",
    ];
    for source in controls {
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
        assert_eq!(
            status, GL_TRUE as i32,
            "false operator rejection: {log}\n{source}"
        );
    }
}

#[test]
fn es2_rejects_invalid_storage_qualifier_declarations() {
    let mut context = GlContext::new();
    let invalid = [
        (
            GL_VERTEX_SHADER,
            "void main(){ attribute mediump float val; }",
        ),
        (
            GL_FRAGMENT_SHADER,
            "attribute mediump float val; void main(){}",
        ),
        (
            GL_VERTEX_SHADER,
            "void main(){ uniform mediump float val; }",
        ),
        (
            GL_FRAGMENT_SHADER,
            "void main(){ uniform mediump float val; }",
        ),
        (
            GL_VERTEX_SHADER,
            "void main(){ varying mediump float val; }",
        ),
        (
            GL_FRAGMENT_SHADER,
            "void main(){ varying mediump float val; }",
        ),
        (
            GL_VERTEX_SHADER,
            "invariant attribute mediump float val; void main(){}",
        ),
        (
            GL_VERTEX_SHADER,
            "invariant uniform mediump float val; void main(){}",
        ),
    ];
    for (kind, source) in invalid {
        let (status, log) = compile(&mut context, kind, source);
        assert_eq!(
            status, GL_FALSE as i32,
            "accepted invalid declaration: {source}"
        );
        assert!(!log.is_empty(), "missing declaration diagnostic: {source}");
    }
}

#[test]
fn legal_global_storage_and_invariance_declarations_remain_accepted() {
    let mut context = GlContext::new();
    let controls = [
        (
            GL_VERTEX_SHADER,
            "attribute mediump vec4 position; void main(){}",
        ),
        (
            GL_VERTEX_SHADER,
            "uniform mediump float scale; void main(){}",
        ),
        (GL_FRAGMENT_SHADER, "uniform sampler2D image; void main(){}"),
        (
            GL_VERTEX_SHADER,
            "invariant varying mediump vec2 uv; void main(){}",
        ),
        (GL_FRAGMENT_SHADER, "varying mediump vec2 uv; void main(){}"),
        (GL_VERTEX_SHADER, "invariant gl_Position; void main(){}"),
    ];
    for (kind, source) in controls {
        let (status, log) = compile(&mut context, kind, source);
        assert_eq!(
            status, GL_TRUE as i32,
            "false declaration rejection: {log}\n{source}"
        );
    }
}

#[test]
fn deqp_struct_type_shadowing_compiles() {
    let mut context = GlContext::new();
    for source in [
        "precision mediump float; bool isOk(int a,int b){return a==b;} varying float v_in0; uniform int ref_out0; int out0; struct S{int val;}; void main(){int in0=int(v_in0*1.0025); int S=S(in0).val; out0=S; bool RES=isOk(out0,ref_out0); gl_FragColor=vec4(RES);}",
        "precision mediump float; struct S{int val;}; void main(){S S=S(1); gl_FragColor=vec4(float(S.val));}",
        "precision mediump float; struct S{int val;}; int func(int S){return S;} void main(){gl_FragColor=vec4(float(func(1)));}",
    ] {
        let (status, log) = compile(&mut context, GL_FRAGMENT_SHADER, source);
        assert_eq!(status, GL_TRUE as i32, "false rejection: {log}\n{source}");
    }
}

#[test]
fn deqp_sampler_cube_uniform_value_shader_compiles() {
    let mut context = GlContext::new();
    let source = "attribute highp vec4 a_position; varying mediump float v_vtxOut; uniform mediump samplerCube u_var; mediump float compare_float(mediump float a, mediump float b){return abs(a-b)<0.05?1.0:0.0;} mediump float compare_vec4(mediump vec4 a, mediump vec4 b){return compare_float(a.x,b.x)*compare_float(a.y,b.y)*compare_float(a.z,b.z)*compare_float(a.w,b.w);} void main(){gl_Position=a_position;v_vtxOut=1.0;v_vtxOut*=compare_vec4(textureCube(u_var,vec3(0.0)),vec4(0.28,0.51,0.88,0.18));}";
    let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
    assert_eq!(status, GL_TRUE as i32, "false rejection: {log}");
}

#[test]
fn deqp_boolean_uniform_array_value_shader_compiles() {
    let mut context = GlContext::new();
    for source in [
        "attribute highp vec4 a_position; varying mediump float v_vtxOut; uniform bool u_var[3]; mediump float compare_bool(bool a, bool b){return a==b?1.0:0.0;} void main(){gl_Position=a_position;v_vtxOut=1.0;v_vtxOut*=compare_bool(u_var[0],false);v_vtxOut*=compare_bool(u_var[1],true);v_vtxOut*=compare_bool(u_var[2],true);}",
        "uniform vec4 values[2]; void take(float value){} void main(){take(values[0].x);}",
        "uniform bvec4 values[2]; void take(bool value){} void main(){take(values[0][1]);}",
    ] {
        let (status, log) = compile(&mut context, GL_VERTEX_SHADER, source);
        assert_eq!(status, GL_TRUE as i32, "false rejection: {log}\n{source}");
    }
}
