pub struct Runtime;

impl Runtime {
    /// Makes GTK runtime data discoverable before GTK/GIO initialize.
    pub fn configure() {
        if std::env::var_os("GSETTINGS_SCHEMA_DIR").is_none() {
            if let Some(directory) = std::env::current_exe()
                .ok()
                .and_then(|executable| Self::bundled_schemas(&executable))
            {
                std::env::set_var("GSETTINGS_SCHEMA_DIR", directory);
                return;
            }
        }

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

    fn bundled_schemas(executable: &std::path::Path) -> Option<std::path::PathBuf> {
        let macos = executable.parent()?;
        if macos.file_name()? != "MacOS" {
            return None;
        }
        let directory = macos.parent()?.join("Resources/glib-2.0/schemas");
        directory
            .join("gschemas.compiled")
            .is_file()
            .then_some(directory)
    }
}

#[cfg(test)]
mod tests {
    use super::Runtime;
    use std::path::Path;

    #[test]
    fn bundled_schemas_are_resolved_beside_the_application_executable() {
        let root = tempfile::tempdir().unwrap();
        let contents = root.path().join("Husklet.app/Contents");
        let executable = contents.join("MacOS/husklet");
        let schemas = contents.join("Resources/glib-2.0/schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        std::fs::write(schemas.join("gschemas.compiled"), "schema index").unwrap();

        assert_eq!(Runtime::bundled_schemas(&executable), Some(schemas));
        assert_eq!(
            Runtime::bundled_schemas(Path::new("/usr/bin/husklet")),
            None
        );
    }
}
