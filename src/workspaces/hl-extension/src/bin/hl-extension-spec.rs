fn main() {
    let document = hl_extension::specification::document();
    if std::env::args().nth(1).as_deref() == Some("--write") {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("protocol");
        let fingerprint = hl_extension::specification::fingerprint([document.as_bytes()]);
        std::fs::write(directory.join("v1.json"), document).expect("write protocol specification");
        std::fs::write(directory.join("v1.fnv1a64"), format!("{fingerprint:016x}\n"))
            .expect("write protocol specification fingerprint");
    } else {
        print!("{document}");
    }
}
