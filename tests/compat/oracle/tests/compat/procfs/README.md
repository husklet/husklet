# Linux pseudo-filesystem compatibility

This suite transfers every C source and support file from the legacy procfs
group without byte changes. Each C fixture is built for both Linux guest ISAs
and is expected to produce the exact verdict in `expected/shared` through the
matching production engine. Portable cases must also agree byte-for-byte
across engines. The suite is Linux-only and has no native-oracle path.

`manifest.tsv` covers the 22 compiled registrations. `images.tsv` separately
preserves the three image-rootfs shell registrations because they are external
image integration cases, not C guest programs. The old `HL_VOL` spelling is
recorded there; the compiled bind-mount case uses the production
`HL_VOLUMES` option.
