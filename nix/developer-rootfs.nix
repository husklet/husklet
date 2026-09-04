{ pkgs, alpineArchives }:

let
  lib = pkgs.lib;
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
  incomplete = name: {
    inherit name;
    version = null;
    url = null;
    sha256 = null;
  };
  manifests = {
    arm64 = {
      machine = 183;
      machineName = "AArch64";
      closureComplete = false;
      packages = map incomplete [ "build-base" "git" "ripgrep" ];
    };
    amd64 = {
      machine = 62;
      machineName = "Advanced Micro Devices X86-64";
      closureComplete = false;
      packages = map incomplete [ "build-base" "git" "ripgrep" ];
    };
  };

  missingFields = manifest:
    lib.concatMap
      (package:
        map (field: "${package.name}.${field}")
          (lib.filter (field: package.${field} == null) [ "version" "url" "sha256" ]))
      manifest.packages;

  complete = manifest: manifest.closureComplete && missingFields manifest == [ ];

  forArchitecture = architecture:
    let
      manifest = manifests.${architecture} or (throw "unsupported developer rootfs architecture: ${architecture}");
      missing = missingFields manifest;
    in
    if !manifest.closureComplete || missing != [ ] then
      throw "developer rootfs ${architecture} APK closure is incomplete (closureComplete=${toString manifest.closureComplete}): ${lib.concatStringsSep ", " missing}"
    else
      let
        apks = map
          (package: pkgs.fetchurl {
            name = "${package.name}-${package.version}.apk";
            inherit (package) url sha256;
          })
          manifest.packages;
      in
      pkgs.runCommand "husklet-developer-rootfs-${architecture}" {
        nativeBuildInputs = [ pkgs.binutils pkgs.coreutils pkgs.libarchive ];
      } ''
        set -eu
        mkdir -p "$out"
        bsdtar -xf ${alpineArchives.${architecture}} -C "$out"
        ${lib.concatMapStringsSep "\n" (apk: ''
          bsdtar -xf ${apk} -C "$out" --exclude .PKGINFO --exclude .SIGN.RSA.*
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
