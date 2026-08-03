#![cfg(target_os = "macos")]

use naga::valid::{Capabilities, ValidationFlags, Validator};

#[test]
fn matrix_negation_is_lowered_to_an_operation_metal_accepts() {
    let mut module = naga::Module::default();
    let matrix = module.types.insert(
        naga::Type {
            name: None,
            inner: naga::TypeInner::Matrix {
                columns: naga::VectorSize::Bi,
                rows: naga::VectorSize::Bi,
                scalar: naga::Scalar::F32,
            },
        },
        Default::default(),
    );
    let mut function = naga::Function {
        name: Some("negate_matrix".into()),
        arguments: vec![naga::FunctionArgument {
            name: Some("value".into()),
            ty: matrix,
            binding: None,
        }],
        result: Some(naga::FunctionResult {
            ty: matrix,
            binding: None,
        }),
        ..Default::default()
    };
    let value = function
        .expressions
        .append(naga::Expression::FunctionArgument(0), Default::default());
    let negated = function.expressions.append(
        naga::Expression::Unary {
            op: naga::UnaryOperator::Negate,
            expr: value,
        },
        Default::default(),
    );
    function.body.push(
        naga::Statement::Emit(function.expressions.range_from(1)),
        Default::default(),
    );
    function.body.push(
        naga::Statement::Return {
            value: Some(negated),
        },
        Default::default(),
    );
    let _ = module.functions.append(function, Default::default());

    let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .unwrap();
    let (msl, _) =
        naga::back::msl::write_string(&module, &info, &Default::default(), &Default::default())
            .unwrap();

    assert!(msl.contains("value * -1.0"), "{msl}");
    assert!(!msl.contains("-(value)"), "{msl}");
}
