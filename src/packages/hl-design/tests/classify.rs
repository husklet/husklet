#[hl_design::classify(root = fs)]
fn root(value: &str) -> &str {
    value
}

#[hl_design::classify(domain = gpu)]
fn domain(value: u32) -> u32 {
    value
}

#[hl_design::classify(pkg)]
fn package(value: f64) -> f64 {
    value
}

#[hl_design::classify(struct = Path)]
fn entity(value: &str) -> &str {
    value
}

#[hl_design::naming(reason = "external protocol terminology")]
struct Updated;

struct State<T>(T);

#[hl_design::adapter]
fn handler(State(value): State<u32>) -> u32 {
    value
}

struct Example;

impl Example {
    #[hl_design::naming(reason = "builder vocabulary established by the public API")]
    fn image(self, _value: &str) -> Self {
        self
    }
}

#[test]
fn classifications_preserve_functions() {
    assert_eq!(root("path"), "path");
    assert_eq!(domain(7), 7);
    assert_eq!(package(2.0), 2.0);
    assert_eq!(entity("path"), "path");
    assert_eq!(handler(State(9)), 9);
    let _ = Example.image("image");
    let _ = Updated;
}
