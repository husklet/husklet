use std::{collections::BTreeMap, env, fs, path::Path};

#[derive(Clone)]
struct Field {
    name: String,
    offset: usize,
    count: usize,
    lanes: usize,
}

fn main() {
    let arguments: Vec<_> = env::args().skip(1).collect();
    let check = arguments.iter().any(|argument| argument == "--check");
    let root = arguments
        .iter()
        .find(|argument| argument.as_str() != "--check")
        .cloned()
        .unwrap_or_else(|| ".".into());
    let root = Path::new(&root);
    let source = fs::read_to_string(root.join("layout.tsv")).expect("read layout.tsv");
    let mut layouts: BTreeMap<String, Vec<Field>> = BTreeMap::new();
    for (line_number, line) in source.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let columns: Vec<_> = line.split('\t').collect();
        assert!(columns.len() == 5, "invalid schema line {}", line_number + 1);
        let lanes = columns[4]
            .strip_prefix("u64x")
            .map_or_else(|| (columns[4] == "u64").then_some(1), |value| value.parse().ok())
            .filter(|lanes| *lanes != 0)
            .unwrap_or_else(|| panic!("invalid scalar on schema line {}", line_number + 1));
        layouts.entry(columns[0].into()).or_default().push(Field {
            name: columns[1].into(),
            offset: columns[2].parse().expect("numeric offset"),
            count: columns[3].parse().expect("numeric count"),
            lanes,
        });
    }
    for fields in layouts.values() {
        let mut expected = 0;
        for field in fields {
            assert_eq!(field.offset, expected, "schema contains padding before {}", field.name);
            expected += field.count * field.lanes * 8;
        }
    }
    output(
        &root.join("../../runtime/native/cpu/include/layout.h"),
        c_output(&layouts),
        check,
    );
    output(&root.join("rust/layout.rs"), rust_output(&layouts), check);
}

fn output(path: &Path, generated: String, check: bool) {
    if check {
        let current = fs::read_to_string(path).expect("read generated output");
        assert_eq!(current, generated, "generated output drifted: {}", path.display());
    } else {
        fs::write(path, generated).expect("write generated output");
    }
}

fn c_output(layouts: &BTreeMap<String, Vec<Field>>) -> String {
    let mut out = String::from(
        concat!(
            "/* Generated from src/native/cpu/layout.tsv by generate.rs. */\n",
            "#ifndef HL_NATIVE_CPU_LAYOUT_H\n#define HL_NATIVE_CPU_LAYOUT_H\n\n",
            "#include <stddef.h>\n#include <stdint.h>\n\n",
        ),
    );
    for (architecture, fields) in layouts {
        out.push_str(&format!("typedef struct hl_native_{architecture}_cpu {{\n"));
        for field in fields {
            let qualifier = if field.name == "interrupt" { "volatile " } else { "" };
            let count = match (field.count, field.lanes) {
                (1, 1) => String::new(),
                (count, 1) => format!("[{count}]"),
                (1, lanes) => format!("[{lanes}]"),
                (count, lanes) => format!("[{count}][{lanes}]"),
            };
            out.push_str(&format!("    {qualifier}uint64_t {}{count};\n", field.name));
        }
        out.push_str(&format!("}} hl_native_{architecture}_cpu;\n\n"));
    }
    out.push_str(
        r#"#define HL_CPU_ASSERT(type, field, expected) \
    _Static_assert(offsetof(type, field) == (expected), #type "." #field " offset drifted")

"#,
    );
    for (architecture, fields) in layouts {
        for field in fields {
            out.push_str(&format!(
                "HL_CPU_ASSERT(hl_native_{architecture}_cpu, {}, {});\n",
                field.name, field.offset
            ));
        }
        let size = fields.last().map_or(0, |field| field.offset + field.count * field.lanes * 8);
        out.push_str(&format!(
            "_Static_assert(sizeof(hl_native_{architecture}_cpu) == {size}, \
             \"{architecture} native CPU prefix size drifted\");\n\n"
        ));
        out.push_str(&format!(
            "_Static_assert(_Alignof(hl_native_{architecture}_cpu) == 8, \
             \"{architecture} native CPU prefix alignment drifted\");\n\n"
        ));
    }
    out.push_str("#undef HL_CPU_ASSERT\n#endif\n");
    out
}

fn rust_output(layouts: &BTreeMap<String, Vec<Field>>) -> String {
    let mut out = String::from("// Generated from ../layout.tsv by ../generate.rs.\n\n");
    for (architecture, fields) in layouts {
        let name = if architecture == "aarch64" { "Aarch64Cpu" } else { "X86_64Cpu" };
        out.push_str(&format!("#[repr(C)]\npub struct {name} {{\n"));
        for field in fields {
            let value = match (field.count, field.lanes) {
                (1, 1) => "u64".into(),
                (count, 1) => format!("[u64; {count}]"),
                (1, lanes) => format!("[u64; {lanes}]"),
                (count, lanes) => format!("[[u64; {lanes}]; {count}]"),
            };
            out.push_str(&format!("    pub {}: {value},\n", field.name));
        }
        out.push_str("}\n\n");
    }
    out.push_str("const _: () = {\n");
    for (architecture, fields) in layouts {
        let name = if architecture == "aarch64" { "Aarch64Cpu" } else { "X86_64Cpu" };
        for field in fields {
            out.push_str(&format!(
                "    assert!(std::mem::offset_of!({name}, {}) == {});\n",
                field.name, field.offset
            ));
        }
        let size = fields.last().map_or(0, |field| field.offset + field.count * field.lanes * 8);
        out.push_str(&format!("    assert!(std::mem::size_of::<{name}>() == {size});\n\n"));
        out.push_str(&format!("    assert!(std::mem::align_of::<{name}>() == 8);\n\n"));
    }
    out.push_str("};\n");
    out
}
