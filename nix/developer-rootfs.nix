{ pkgs, alpineArchives }:

let
  lib = pkgs.lib;
  # APK_HASHES_START -- updated only by nix/update-developer-rootfs-apks.py output.
  payloadHashes = {
    arm64 = { };
    amd64 = { };
  };
  # APK_HASHES_END
  requiredTools = [
    "bin/sh"
    "usr/bin/git"
    "usr/bin/gcc"
    "usr/bin/rg"
    "usr/bin/make"
    "usr/bin/ar"
    "bin/tar"
    "sbin/apk"
  ];

  # This is an APK *closure* manifest, not merely a list of requested packages.
  # Add every transitive APK in dependency-first extraction order, using URLs
  # from one immutable Alpine 3.24 repository snapshot and hashes from `nix hash`.
  # Solved by Alpine apk-tools 3.24.1 against the signed indexes below. This is
  # dependency-first order; sizes are APKINDEX S fields, not measured payloads.
  closure = [
    [ "libgcc" "15.2.0-r5" "main" 80348 66323 ]
    [ "jansson" "2.15.0-r0" "main" 48950 49825 ]
    [ "libstdc++" "15.2.0-r5" "main" 961295 908984 ]
    [ "zstd-libs" "1.5.7-r2" "main" 376646 365383 ]
    [ "binutils" "2.45.1-r1" "main" 3229647 3321769 ]
    [ "libmagic" "5.47-r2" "main" 526164 527434 ]
    [ "file" "5.47-r2" "main" 9722 10233 ]
    [ "libgcc-static" "15.2.0-r5" "main" 2249602 2187958 ]
    [ "libgomp" "15.2.0-r5" "main" 153729 146527 ]
    [ "libatomic" "15.2.0-r5" "main" 10241 10362 ]
    [ "gmp" "6.3.0-r4" "main" 220074 221014 ]
    [ "isl26" "0.26-r2" "main" 915985 870482 ]
    [ "mpfr4" "4.2.2-r0" "main" 313348 294772 ]
    [ "mpc1" "1.3.1-r1" "main" 48549 53688 ]
    [ "gcc" "15.2.0-r5" "main" 60496469 52501295 ]
    [ "libstdc++-dev" "15.2.0-r5" "main" 4145023 4132711 ]
    [ "musl-dev" "1.2.6-r2" "main" 3421233 2539434 ]
    [ "g++" "15.2.0-r5" "main" 18449957 17241310 ]
    [ "make" "4.4.1-r4" "main" 116285 117543 ]
    [ "fortify-headers" "3.0.1-r2" "main" 7356 7328 ]
    [ "patch" "2.8-r0" "main" 69750 70892 ]
    [ "build-base" "0.5-r4" "main" 1340 1325 ]
    [ "brotli-libs" "1.2.0-r1" "main" 429608 424924 ]
    [ "c-ares" "1.34.8-r0" "main" 110689 108771 ]
    [ "libunistring" "1.4.2-r0" "main" 743597 746169 ]
    [ "libidn2" "2.3.8-r0" "main" 104623 105430 ]
    [ "nghttp2-libs" "1.69.0-r0" "main" 63231 63631 ]
    [ "libpsl" "0.21.5-r3" "main" 54692 55302 ]
    [ "libcurl" "8.22.0-r0" "main" 358307 358024 ]
    [ "libexpat" "2.8.4-r0" "main" 61243 60700 ]
    [ "pcre2" "10.48-r0" "main" 322467 320066 ]
    [ "git" "2.54.0-r0" "main" 3546724 3594478 ]
    [ "git-init-template" "2.54.0-r0" "main" 10825 10812 ]
    [ "ripgrep" "15.1.0-r0" "community" 1386969 1336743 ]
  ];
  package = architecture: entry: {
    name = builtins.elemAt entry 0;
    version = builtins.elemAt entry 1;
    repository = builtins.elemAt entry 2;
    expectedSize = builtins.elemAt entry (if architecture == "amd64" then 3 else 4);
    url = "https://dl-cdn.alpinelinux.org/alpine/v3.24/${builtins.elemAt entry 2}/${if architecture == "amd64" then "x86_64" else "aarch64"}/${builtins.elemAt entry 0}-${builtins.elemAt entry 1}.apk";
    sha256 = payloadHashes.${architecture}.${builtins.elemAt entry 0} or null;
  };
  manifests = {
    arm64 = {
      machine = 183;
      machineName = "AArch64";
      expectedDownloadSize = 92831642;
      indexes = {
        main = { url = "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/APKINDEX.tar.gz"; sha256 = "6ad66a68b4c9d08a5b64f19f6b0de56831e1e8505d15d6dbb6f7c92abc4b442f"; };
        community = { url = "https://dl-cdn.alpinelinux.org/alpine/v3.24/community/aarch64/APKINDEX.tar.gz"; sha256 = "cc6373bb1887941bdd1b7cade9f53455e3f2b4762ee072b020a36e691f6cd451"; };
      };
      # Set only after inspecting every fixed APK control stream for scripts/triggers.
      scriptsAudited = false;
      closureComplete = false;
      packages = map (package "arm64") closure;
    };
    amd64 = {
      machine = 62;
      machineName = "Advanced Micro Devices X86-64";
      expectedDownloadSize = 103044688;
      indexes = {
        main = { url = "https://dl-cdn.alpinelinux.org/alpine/v3.24/main/x86_64/APKINDEX.tar.gz"; sha256 = "d06e04b2a46b8f669cd1fa5244ed3786a63efd41d9a2403433a9e6a21d9d7ce1"; };
        community = { url = "https://dl-cdn.alpinelinux.org/alpine/v3.24/community/x86_64/APKINDEX.tar.gz"; sha256 = "d5fda4c2f0c2ed5c537a1a3aeeb7e642e5aabf7e22b2ae2be9d99d341f1fcef7"; };
      };
      # Set only after inspecting every fixed APK control stream for scripts/triggers.
      scriptsAudited = false;
      closureComplete = false;
      packages = map (package "amd64") closure;
    };
  };

  missingFields = manifest:
    lib.concatMap
      (package:
        map (field: "${package.name}.${field}")
          (lib.filter (field: package.${field} == null) [ "version" "url" "sha256" ]))
      manifest.packages;

  complete = manifest: manifest.closureComplete && manifest.scriptsAudited && missingFields manifest == [ ];

  forArchitecture = architecture:
    let
      manifest = manifests.${architecture} or (throw "unsupported developer rootfs architecture: ${architecture}");
      missing = missingFields manifest;
    in
    if !manifest.closureComplete || !manifest.scriptsAudited || missing != [ ] then
      throw "developer rootfs ${architecture} APK closure is incomplete or unaudited: ${lib.concatStringsSep ", " missing}"
    else
      let
        apkArchitecture = if architecture == "amd64" then "x86_64" else "aarch64";
        indexes = lib.mapAttrs
          (repository: index: pkgs.fetchurl {
            name = "APKINDEX-${repository}-${apkArchitecture}.tar.gz";
            inherit (index) url sha256;
          })
          manifest.indexes;
        apks = map
          (package: {
            inherit (package) expectedSize name repository version;
            source = pkgs.fetchurl {
              name = "${package.name}-${package.version}.apk";
              inherit (package) url sha256;
            };
          })
          manifest.packages;
      in
      pkgs.runCommand "husklet-developer-rootfs-${architecture}" {
        nativeBuildInputs = [ pkgs.apk-tools pkgs.binutils pkgs.coreutils pkgs.libarchive ];
      } ''
        set -eu
        mkdir -p "$out"
        bsdtar -xf ${alpineArchives.${architecture}} -C "$out"
        mkdir -p repository/main/${apkArchitecture} repository/community/${apkArchitecture}
        ln -s ${indexes.main} repository/main/${apkArchitecture}/APKINDEX.tar.gz
        ln -s ${indexes.community} repository/community/${apkArchitecture}/APKINDEX.tar.gz
        ${lib.concatMapStringsSep "\n" (apk: ''
          test "$(stat -c %s ${apk.source})" = ${toString apk.expectedSize} || {
            echo "APK ${apk.name} size differs from its signed APKINDEX" >&2
            exit 1
          }
          ln -s ${apk.source} repository/${apk.repository}/${apkArchitecture}/${apk.name}-${apk.version}.apk
        '') apks}
        printf '%s\n' "$PWD/repository/main" "$PWD/repository/community" > repositories
        # Payload audit must establish that package scripts and path triggers are
        # unnecessary before scriptsAudited may become true. Disabling them makes
        # this transaction host-independent for the foreign-ISA root.
        apk --root "$out" --arch ${apkArchitecture} --usermode --no-network --no-cache \
          --scripts=false --commit-hooks=false \
          --repositories-file "$PWD/repositories" add \
          build-base=0.5-r4 git=2.54.0-r0 ripgrep=15.1.0-r0
        ${lib.concatMapStringsSep "\n" (apk: ''
          apk --root "$out" --arch ${apkArchitecture} info --exists '${apk.name}=${apk.version}'
        '') apks}
        expected_machine=${toString manifest.machine}
        for tool in ${lib.escapeShellArgs requiredTools}; do
          path="$out/$tool"
          test -x "$path" || { echo "developer rootfs omits executable /$tool" >&2; exit 1; }
        done
        for tool in ${lib.escapeShellArgs requiredTools}; do
          path="$out/$tool"
          links=0
          while test -L "$path"; do
            target=$(readlink "$path")
            case "$target" in
              /*) path="$out$target" ;;
              *) path="$(dirname "$path")/$target" ;;
            esac
            links=$((links + 1))
            test "$links" -le 40 || { echo "developer rootfs /$tool has a symlink loop" >&2; exit 1; }
          done
          path=$(realpath -m "$path")
          case "$path" in
            "$out"/*) ;;
            *) echo "developer rootfs /$tool escapes the root" >&2; exit 1 ;;
          esac
          machine=$(od -An -tu2 -j18 -N2 "$path" | tr -d ' ')
          test "$machine" = "$expected_machine" || {
            echo "developer rootfs /$tool has ELF machine $machine, expected $expected_machine (${manifest.machineName})" >&2
            exit 1
          }
        done
        printf '%s\n' '${architecture}' > "$out/.husklet-developer-rootfs-architecture"
      '';
in
{
  inherit complete forArchitecture manifests missingFields requiredTools;
}
