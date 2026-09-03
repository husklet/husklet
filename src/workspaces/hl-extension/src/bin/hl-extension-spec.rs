fn main() {
    let document = hl_extension::specification::document();
    if std::env::args().nth(1).as_deref() == Some("--write") {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("protocol/v1.json");
        std::fs::write(path, document).expect("write protocol specification");
    } else {
        print!("{document}");
    }
}
