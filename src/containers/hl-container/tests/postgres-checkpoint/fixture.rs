use super::*;

impl Fixture {
    pub(super) async fn new() -> Result<Self, Error> {
        let archive = std::env::var_os("HL_POSTGRES_ROOTFS_ARCHIVE")
            .ok_or("HL_POSTGRES_ROOTFS_ARCHIVE must name a pinned postgres:16-alpine rootfs tar.gz")?;
        let manifest_path = std::env::var_os("HL_POSTGRES_FIXTURE_MANIFEST")
            .ok_or("HL_POSTGRES_FIXTURE_MANIFEST must name the pinned fixture JSON")?;
        let manifest: FixtureManifest = serde_json::from_reader(std::fs::File::open(&manifest_path)?)?;
        let guest = guest()?;
        let expected_arch = match guest {
            Guest::X86_64 => "amd64",
            Guest::Aarch64 => "arm64",
        };
        require(
            manifest.postgres_major == 16,
            "fixture manifest must pin PostgreSQL major 16",
        )?;
        require(
            manifest.image == "postgres:16-alpine",
            format!("unexpected fixture image {}", manifest.image),
        )?;
        require(
            manifest.image_digest.starts_with("sha256:") && manifest.image_digest.len() == 71,
            "fixture image_digest must be a sha256 digest",
        )?;
        require(
            manifest.image_digest[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "fixture image_digest must contain 64 lowercase hexadecimal digits",
        )?;
        require(
            manifest.archive_sha256.len() == 64
                && manifest
                    .archive_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "fixture archive_sha256 must contain 64 lowercase hexadecimal digits",
        )?;
        let expected_image_digest = match expected_arch {
            "amd64" => POSTGRES_AMD64_DIGEST,
            "arm64" => POSTGRES_ARM64_DIGEST,
            _ => unreachable!("guest() only returns supported architectures"),
        };
        require(
            manifest.image_digest == expected_image_digest,
            "fixture manifest image digest differs from independent pin",
        )?;
        require(
            manifest.postgres_version.starts_with("16."),
            "fixture must pin an exact PostgreSQL 16 patch version",
        )?;
        require(
            manifest.architecture == expected_arch,
            format!(
                "fixture architecture {} does not match {expected_arch}",
                manifest.architecture
            ),
        )?;
        require(
            file_hash(Path::new(&archive))? == manifest.archive_sha256,
            "PostgreSQL fixture archive digest mismatch",
        )?;
        let work = tempfile::tempdir()?;
        let rootfs = work.path().join("rootfs");
        std::fs::create_dir(&rootfs)?;
        let input = std::fs::File::open(&archive).map_err(|error| {
            format!(
                "open PostgreSQL rootfs archive {}: {error}",
                Path::new(&archive).display()
            )
        })?;
        tar::Archive::new(flate2::read::GzDecoder::new(input)).unpack(&rootfs)?;
        for required in [
            "usr/local/bin/docker-entrypoint.sh",
            "usr/local/bin/postgres",
            "usr/local/bin/psql",
        ] {
            require(
                rootfs.join(required).is_file(),
                format!("fixture is not PostgreSQL: missing /{required}"),
            )?;
        }
        verify_elf_machine(&rootfs.join("usr/local/bin/postgres"), expected_arch)?;
        let state = work.path().join("state");
        let containers = Containers::builder(Config::new(&state)).build().await?;
        Ok(Self {
            _work: work,
            rootfs,
            state,
            containers,
            guest,
            postgres_version: manifest.postgres_version,
        })
    }
}
