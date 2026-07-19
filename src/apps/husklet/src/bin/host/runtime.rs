pub struct Runtime;

impl Runtime {
    /// Makes GTK runtime data discoverable before GTK/GIO initialize.
    pub fn configure() {
        if let Some(paths) = std::env::var_os("GSETTINGS_SCHEMAS_PATH") {
            // Nix exposes each schema package as a data root containing
            // `glib-2.0/schemas/gschemas.compiled`. GIO supports multiple roots through
            // XDG_DATA_DIRS; GSETTINGS_SCHEMA_DIR only supports one and would hide GTK's
            // own file/color chooser schemas.
            let mut roots = paths;
            if let Some(existing) = std::env::var_os("XDG_DATA_DIRS") {
                roots.push(":");
                roots.push(existing);
            }
            std::env::set_var("XDG_DATA_DIRS", roots);
            return;
        }

        if std::env::var_os("GSETTINGS_SCHEMA_DIR").is_none() {
            let Some(root) = std::env::var_os("HL_GSETTINGS_SCHEMAS") else {
                return;
            };
            let root = std::path::PathBuf::from(root);
            let direct = root.join("share/glib-2.0/schemas");
            if direct.join("gschemas.compiled").is_file() {
                std::env::set_var("GSETTINGS_SCHEMA_DIR", direct);
            }
        }
    }
}
