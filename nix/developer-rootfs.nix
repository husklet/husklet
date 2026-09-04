{ pkgs, alpineArchives }:

let
  lib = pkgs.lib;
  # APK_HASHES_START -- updated only by nix/update-developer-rootfs-apks.py output.
  payloadHashes = {
    arm64 = {
      "libgcc" = "sha256-Npqqbp0JmnN7rW3T5sL+e7FUfKJtIrlO4EESKPcJtAM=";
      "jansson" = "sha256-oTgi9coXcsztS7fT0XjsXe53DYrMultIFrLdxBPm8FU=";
      "libstdc++" = "sha256-IwLnZtTkkmA47BZuy4WDfuiEV2EVI23bVl46X8pKEdc=";
      "zstd-libs" = "sha256-K7UTbIn1sLvhVUyJFaO1INWqY64qUdTYIeuBaY21qBg=";
      "binutils" = "sha256-usSgDnMpbf6/X1h8KzBLfREOfeZYzSsFSR+HwrIoQgw=";
      "libmagic" = "sha256-vNIz8opvHAfNeJxH3yI06zXSusKVqiI+oszvF9dc4PA=";
      "file" = "sha256-+xzMUgwugMsdO3OVAIJs7fSzW4pD/3iqaDxva9SN0+U=";
      "libgcc-static" = "sha256-LsPcS+lT2ELMURkySDZ9hoKe1pmh7QlPtRSbGz/GKRk=";
      "libgomp" = "sha256-kFdqiJnFYNOYO4rIRCtbdZV6kXvBxOlQ4AWUUID4aC8=";
      "libatomic" = "sha256-MaRf7AU8Q6AGNw+DI48W+VmDippwP78mv9GjbaT1OLI=";
      "gmp" = "sha256-TatuhrhQ/htbwM8vrBfvpNf2SL6kTz2QGWlMweHLErc=";
      "isl26" = "sha256-wZXqJpKa2iu55mt7xtZLNAEjWRuPTI7RWGk+TawJI9g=";
      "mpfr4" = "sha256-gcsnbB4FATj+2QyPHvWEz1v84mc5czdT/OE4/AoJKzU=";
      "mpc1" = "sha256-xOA5I88blYug02ZRlvUhoA+wkRMuecOUkymAGftZrmA=";
      "gcc" = "sha256-/z/s/DLJob7UpHoyzP7m9vV1cIJY+RLCGNzo7Q2JIiE=";
      "libstdc++-dev" = "sha256-g4XIEdk7ZO4YdwBaVHPN6Our6C00YwHysVP/jLanFRA=";
      "musl-dev" = "sha256-ynK4T06zw2u77ATZGesQFZ1BTHpDQTzCJThB0wH6lIY=";
      "g++" = "sha256-3Ba6vW/dcVTs4mPKYl/4MrA1KNWPUGD56SyjsLwMB5o=";
      "make" = "sha256-CixsP6DZC2UruurDYtG/5Ut1quV2Maa0SIJvi1HIUXA=";
      "fortify-headers" = "sha256-cr8qd0dfJVCnNFaDx3s5SH7pTsvCyhHtUxQWHzuSfXY=";
      "patch" = "sha256-xse/Cl16mgbW1uTb9Eq2sxMIRyug4VyhsSDyKBs1zwo=";
      "build-base" = "sha256-x8p3a03EBE00qfCJ1MeKx4f7yTdIX3aA1Dtu30wis4Y=";
      "brotli-libs" = "sha256-4OeniS8oMmqfGOCWB7Gupyuu3aRwvE49KxEmK5rl3aM=";
      "c-ares" = "sha256-aJYD0wDcVvC5njE1BuTXs6P/uHzsU2Mk+KMGvCGK/mc=";
      "libunistring" = "sha256-WKv8vOJFkHHFBXcaVyp5lT1gQ9crZD7Q1Shco5za+Fw=";
      "libidn2" = "sha256-0C/9pFEGUvicX7ic0uf/chr7DHTn11R2rWC+vnrAriA=";
      "nghttp2-libs" = "sha256-EOyVD8pSbbh7vcBwK6Yj/KZjnfFau0gin1SNaBJWOzM=";
      "libpsl" = "sha256-sx6RNxQl1fgMHbXLoqhJlWtlyAKt0LSd6ESyypYaqNo=";
      "libcurl" = "sha256-y+yDCmOIi+VwwG0g2HyFZJr/HbOMO03MF4hLFEceOeM=";
      "libexpat" = "sha256-yhvlvjaYWoNw9Kjp4zoleIrJUmApVxjhsLchoWuyqmE=";
      "pcre2" = "sha256-DopCXsI/yhtUnoN1izhecYagcFl+jcAUFjRi+WAHkWc=";
      "git" = "sha256-6kHi+Dfx9qi2bnKl7OD8dcykT3b0nPVQDUZ/xWJ9Crc=";
      "git-init-template" = "sha256-utFj2zC/2QyZ+G+sVOesCCBAAk0hTA4dDH2YpPWn7bU=";
      "ripgrep" = "sha256-G/Z8+kRkvJHh3RpPU3xAZkxHPOkr8W67pkvHlo11Res=";
    };
    amd64 = {
      "libgcc" = "sha256-OT3NMmKfBtfYVAnCctFC0MCCdy0QuH71XugvR9475jc=";
      "jansson" = "sha256-doG0FIbh8rLvzhGHXIOVM0ZK6IV9MPxJEw8AOcZYX9s=";
      "libstdc++" = "sha256-FMmHtVb1OFpdsYN254jHXzfYUyG43Bkg2SbqfarB1vY=";
      "zstd-libs" = "sha256-I8YGWwBJskBkQVZLzwAyUVpD946A12/LhVNaOAPvXU4=";
      "binutils" = "sha256-FWgoh0WdIWWF/rQ4u/t+xj4fM+d8RT9EY30tnYw1l30=";
      "libmagic" = "sha256-GCCs0pkCVhmoSLGayYx5lv2SwGllJViYgAtqeQqLA6I=";
      "file" = "sha256-RoHBU8UJrsEm4rnqLJWXMDCbJKeS1J/IzCb6X6h+j90=";
      "libgcc-static" = "sha256-VjJEpWL1xIuSH38LSlceNzz8umUXX+LU91LF+XVzi80=";
      "libgomp" = "sha256-5yBKrQDjHVROCjTqxafrfTWG0reyKv1uWQcYnE4DN8w=";
      "libatomic" = "sha256-xexVDHPde06B4gb9/zz0xGsK4bKf7B21zU4S2IQWq78=";
      "gmp" = "sha256-hTI+9bzo/yNHNtwtZZLqN669xjSKUx4nQwvf44Nk0lY=";
      "isl26" = "sha256-MJmftm4pbI6nqhAsrQ0wlVGbMCKrtiLSlR3Sk2cxDqk=";
      "mpfr4" = "sha256-k2IKgD1BbowFFQ+xRBN5bfFOo3AyLf9r0QZFXeqxVkY=";
      "mpc1" = "sha256-wiR0OOFj8cEWRkKPB1ds6zzzFn+amlxlJgIfd413rbY=";
      "gcc" = "sha256-0dSCRy7ZEkBzHGDbroF2vB4y4d+2lIVtpnWfbTJxYyU=";
      "libstdc++-dev" = "sha256-Ys4wjNvxJ9ysbu4VIKT/7I3JizCXRlOQiEL+TMfZC0M=";
      "musl-dev" = "sha256-aDHoueSCHa4skSHwZB+B5UP0rCfAMUTEh2mA7oTmmI8=";
      "g++" = "sha256-NcOejTZn2GA/4gnbr+Z2gcZLHNpmv5yDa/tNBbGacf0=";
      "make" = "sha256-qRZiaodvZMa2+zKmvw+pM1HQn64Kp2hBNWQjkNpkfmA=";
      "fortify-headers" = "sha256-X/rr2fFBrD4/oMyZ95j2S0wKoW+wanp35o9j81D+I6o=";
      "patch" = "sha256-cjwGHqPiw+YbRX+PYU8Lh3JK6uN1ZLpMG91BxyIRgYc=";
      "build-base" = "sha256-OscudOlhUJlejXl7bDP8O0bWnjZnxHvQ/yXOifn8Tq8=";
      "brotli-libs" = "sha256-8II958bUWFmutqf6z8XPvy2tuDrU8nx5TLbsx05XjO4=";
      "c-ares" = "sha256-Ej/ercXsiEuJd4f5jYZUkr9wQCaXslHNHzWRoAuMnXM=";
      "libunistring" = "sha256-OqYDDmA+NNlhknQvMnwFCfQJxnac4uexRjaF3t441xQ=";
      "libidn2" = "sha256-fQOKxVjd5GSWSwM85RJt0ur4TMQU3BXnfFQkoyHQ6+4=";
      "nghttp2-libs" = "sha256-fXcFzqRkOJmJ08prlQXb/OPPJc/KHusGcJqZCfk/TxE=";
      "libpsl" = "sha256-7GZaNEtuiH8A8KRwFcxzLDooG2noRkHchmLqI0/ebNs=";
      "libcurl" = "sha256-QLKo4VqNJLyVpfQqrrAYMTof9FfO5x2OFarxMo2nZeg=";
      "libexpat" = "sha256-NPpB45lNmoRPUKfAHLQLsuJy+n9Qa5bpKitKILI6gbw=";
      "pcre2" = "sha256-z/rtts9ARkdhmosDI5WNKT2c0kDo+IfBa99LCgOfmTo=";
      "git" = "sha256-WixngndNLokfwvw/3FDDwnQ7jrLcfNpptrcjry7tz3g=";
      "git-init-template" = "sha256-6AlKR7+m5mt9VQrhKnqLyrC8SvUCqs1dKm/GLSxeSI0=";
      "ripgrep" = "sha256-BVsZc6Xy+s4AXRZ9h07vilNNSx7bIM5gHkbMJTAWsFE=";
    };
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
      scriptsAudited = true;
      closureComplete = true;
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
      scriptsAudited = true;
      closureComplete = true;
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
        hostArchitecture =
          if pkgs.stdenv.hostPlatform.system == "x86_64-linux" then "amd64"
          else if pkgs.stdenv.hostPlatform.system == "aarch64-linux" then "arm64"
          else throw "developer rootfs construction requires an AArch64 or x86-64 Linux host";
        hostLoader = if hostArchitecture == "amd64" then "ld-musl-x86_64.so.1" else "ld-musl-aarch64.so.1";
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
      pkgs.runCommand "husklet-developer-rootfs-${architecture}" { } ''
        set -eu
        mkdir -p "$out"
        tar -xzf ${alpineArchives.${architecture}} -C "$out"
        mkdir apk-host
        tar -xzf ${alpineArchives.${hostArchitecture}} -C apk-host
        apk="$PWD/apk-host/lib/${hostLoader} --library-path $PWD/apk-host/lib:$PWD/apk-host/usr/lib $PWD/apk-host/sbin/apk"
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
        $apk --root "$out" --arch ${apkArchitecture} --usermode --no-network --no-cache \
          --scripts=false --commit-hooks=false \
          --repositories-file "$PWD/repositories" add \
          build-base=0.5-r4 git=2.54.0-r0 ripgrep=15.1.0-r0
        ${lib.concatMapStringsSep "\n" (apk: ''
          $apk --root "$out" --arch ${apkArchitecture} info --exists '${apk.name}=${apk.version}'
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
