{
  description = "Husklet with the integrated C execution engine";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  inputs.rust-overlay = {
    url = "github:oxalica/rust-overlay";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      lib = nixpkgs.lib;
      version = "0.1.0";
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems =
        function:
        lib.genAttrs systems (
          system:
          function (
            import nixpkgs {
              inherit system;
              config.allowUnsupportedSystem = true;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );

      rustFor =
        pkgs:
        pkgs.rust-bin.stable."1.93.1".default.override {
          extensions = [
            "clippy"
            "rustfmt"
          ];
          targets = [
            "aarch64-unknown-linux-gnu"
            "x86_64-unknown-linux-gnu"
            "x86_64-pc-windows-gnu"
          ];
        };

      rustPlatformFor =
        pkgs:
        let
          rust = rustFor pkgs;
        in
        pkgs.makeRustPlatform {
          cargo = rust;
          rustc = rust;
        };

      guestISAs = [
        {
          isa = "aarch64";
          crossAttr = "aarch64-multiplatform";
          loader = "ld-linux-aarch64.so.1";
        }
        {
          isa = "x86_64";
          crossAttr = "gnu64";
          loader = "ld-linux-x86-64.so.2";
        }
      ];

      toolchainFor =
        pkgs:
        let
          host = pkgs.stdenv.hostPlatform;
          hostCpu = host.parsed.cpu.name;
          nativeCC = "${pkgs.stdenv.cc}/bin/cc";
          isNative = guest: host.isLinux && hostCpu == guest.isa;
          pkgsFor = guest: if isNative guest then pkgs else pkgs.pkgsCross.${guest.crossAttr};
          ccFor =
            guest:
            let
              guestPkgs = pkgsFor guest;
            in
            if isNative guest then
              nativeCC
            else
              "${guestPkgs.stdenv.cc}/bin/${guestPkgs.stdenv.cc.targetPrefix}cc";
          ccPackageFor = guest: if isNative guest then pkgs.gcc else (pkgsFor guest).stdenv.cc;
          upper = guest: lib.toUpper guest.isa;
          rustStaticLinkerName = guest: "${guest.isa}-linux-gnu-rust-static-linker";
          rustStaticLinkerFor =
            guest:
            let
              guestPkgs = pkgsFor guest;
            in
            pkgs.writeShellScriptBin (rustStaticLinkerName guest) ''
              static_search=
              for argument in "$@"; do
                if [ "$argument" = -static ] || [ "$argument" = -static-pie ]; then
                  static_search=-L${lib.escapeShellArg "${guestPkgs.glibc.static}/lib"}
                  break
                fi
              done
              exec ${lib.escapeShellArg (ccFor guest)} $static_search "$@"
            '';
          compilerAliasFor =
            guest:
            let
              guestPkgs = pkgsFor guest;
            in
            pkgs.writeShellScriptBin "${guest.isa}-linux-gnu-gcc" ''
              exec ${lib.escapeShellArg (ccFor guest)} -L${lib.escapeShellArg "${guestPkgs.glibc.static}/lib"} "$@"
            '';
        in
        rec {
          canBuildGuests =
            (host.isLinux || host.isDarwin)
            && lib.elem hostCpu [
              "aarch64"
              "x86_64"
            ];
          crossCompilers = map ccPackageFor guestISAs;
          rustStaticLinkers = map rustStaticLinkerFor guestISAs;
          compilerAliases = map compilerAliasFor guestISAs;
          emulators = lib.optional (host.isLinux && hostCpu == "x86_64") pkgs.qemu-user;
          env =
            lib.foldl'
              (
                result: guest:
                let
                  guestPkgs = pkgsFor guest;
                in
                result
                // {
                  "${upper guest}_LINUX_CC" = ccFor guest;
                  "${upper guest}_LINUX_STATIC_CC" = "${ccFor guest} -L${guestPkgs.glibc.static}/lib";
                  "${upper guest}_DYNAMIC_LOADER" = "${guestPkgs.glibc}/lib/${guest.loader}";
                  "${upper guest}_DYNAMIC_LIBC" = "${guestPkgs.glibc}/lib/libc.so.6";
                  "CARGO_TARGET_${
                    lib.toUpper (lib.replaceStrings [ "-" ] [ "_" ] "${guest.isa}-unknown-linux-gnu")
                  }_LINKER" =
                    "${rustStaticLinkerFor guest}/bin/${rustStaticLinkerName guest}";
                  "CC_${lib.replaceStrings [ "-" ] [ "_" ] "${guest.isa}-unknown-linux-gnu"}" = ccFor guest;
                }
              )
              {
                CC = nativeCC;
                NATIVE_CC = nativeCC;
              }
              guestISAs;
        };

      workspaceSource = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          ./Cargo.lock
          ./Cargo.toml
          ./lint.toml
          ./lint
          ./rust-toolchain.toml
          ./rustfmt.toml
          ./src
          ./tests
        ];
      };

      commonNativeInputs =
        pkgs:
        [
          pkgs.bear
          pkgs.clang-tools
          pkgs.cppcheck
          pkgs.pkg-config
        ]
        ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.valgrind ];

      lintCasesFor =
        pkgs:
        pkgs.writeShellApplication {
          name = "husklet-lint-cases";
          runtimeInputs = [ (rustFor pkgs) ];
          text = ''
            if [ ! -f Cargo.toml ] || [ ! -f lint.toml ]; then
              echo 'error: run this command from the Husklet repository root' >&2
              exit 2
            fi

            paths=(src tests)
            if [ "$#" -gt 0 ]; then
              paths=("$@")
            fi
            exec cargo run --locked --offline -q -p hl-design-lint -- \
              --policy lint.toml --cases lint "''${paths[@]}"
          '';
        };

      macBaseFor =
        pkgs:
        pkgs.buildEnv {
          name = "husklet-mac-base";
          ignoreCollisions = true;
          paths = with pkgs; [
            bashInteractive
            coreutils
            gnugrep
            gnused
            gawk
            findutils
            diffutils
            less
            gnutar
            gzip
            which
            ncurses
            cacert
          ];
        };

      macDevFor =
        pkgs:
        pkgs.buildEnv {
          name = "husklet-mac-dev";
          ignoreCollisions = true;
          paths = [
            (macBaseFor pkgs)
          ]
          ++ (with pkgs; [
            zsh
            fish
            git
            curl
            wget
            openssh
            htop
            tree
            jq
            ripgrep
            fd
            fzf
            tmux
            neovim
            gnupg
            pkg-config
            clang
            nodejs
            go
            rustc
            cargo
          ]);
        };

      alpineArchivesFor = pkgs: {
        arm64 = pkgs.fetchurl {
          url = "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/aarch64/alpine-minirootfs-3.24.1-aarch64.tar.gz";
          hash = "sha256-9VqQ9pBSxb1vkssJqPRwZZcIMLGUyRegBvuUAo5yElk=";
        };
        amd64 = pkgs.fetchurl {
          url = "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/alpine-minirootfs-3.24.1-x86_64.tar.gz";
          hash = "sha256-Qfc+PPX6kZuKpcprMNxI8NonIHdtdCPip3SCEUVv4IE=";
        };
      };

      linuxAlpineFor =
        pkgs:
        let
          archives = alpineArchivesFor pkgs;
        in
        if pkgs.stdenv.hostPlatform.system == "aarch64-linux" then
          {
            target = "arm64";
            archive = archives.arm64;
          }
        else if pkgs.stdenv.hostPlatform.system == "x86_64-linux" then
          {
            target = "amd64";
            archive = archives.amd64;
          }
        else
          throw "unsupported Linux Alpine fixture host: ${pkgs.stdenv.hostPlatform.system}";

      packageFor =
        pkgs:
        (rustPlatformFor pkgs).buildRustPackage {
          pname = "hl-engine";
          inherit version;
          src = workspaceSource;
          cargoLock.lockFile = ./Cargo.lock;
          strictDeps = true;
          nativeBuildInputs =
            commonNativeInputs pkgs
            ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.patchelf ]
            ++ lib.optionals pkgs.stdenv.isDarwin [ pkgs.darwin.cctools ];
          doCheck = false;
          buildPhase = ''
            runHook preBuild
            export CARGO_BUILD_JOBS="$NIX_BUILD_CORES"
            export RUSTFLAGS="''${RUSTFLAGS:-} --remap-path-prefix=/build/cargo-vendor-dir=/rust/vendor --remap-path-prefix=$PWD=/rust/source"
            cargo build --release -p engine --bins --locked --offline
            ${lib.optionalString pkgs.stdenv.isLinux ''
              native_libraries=(target/release/build/hl-native-*/out/libhl_native_engine.so)
              if [ "''${#native_libraries[@]}" -ne 1 ] || [ ! -f "''${native_libraries[0]}" ]; then
                printf 'expected exactly one raw Linux native library, found %s\n' \
                  "''${#native_libraries[@]}" >&2
                exit 1
              fi
              raw_native="''${native_libraries[0]}"
              test "$(patchelf --print-soname "$raw_native")" = libhl_native_engine.so
              ! patchelf --print-rpath "$raw_native" | tr : '\n' | grep -F '/build/' >/dev/null
              patchelf --print-needed "$raw_native" | sort -u > "$TMPDIR/raw-native-needed"
              printf '%s\n' \
                ld-linux-aarch64.so.1 \
                ld-linux-x86-64.so.2 \
                libc.so.6 \
                libdl.so.2 \
                libm.so.6 \
                libpthread.so.0 \
                libatomic.so.1 | sort -u > "$TMPDIR/allowed-native-needed"
              while IFS= read -r dependency; do
                grep -Fx "$dependency" "$TMPDIR/allowed-native-needed" >/dev/null || {
                  printf 'unexpected raw native dependency: %s\n' "$dependency" >&2
                  exit 1
                }
              done < "$TMPDIR/raw-native-needed"
              for binary in target/release/hl-engine target/release/hl-aarch64 target/release/hl-x86_64
              do
                ! patchelf --print-rpath "$binary" | tr : '\n' | grep -F '/build/' >/dev/null
                patchelf --print-needed "$binary" | grep -Fx libhl_native_engine.so >/dev/null
              done
            ''}
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            mkdir -p "$out/bin" "$out/lib"
            for binary in hl-engine hl-aarch64 hl-x86_64
            do
              install -Dm755 "target/release/$binary" "$out/bin/$binary"
            done
            native_libraries=(target/release/build/hl-native-*/out/libhl_native_engine.*)
            if [ "''${#native_libraries[@]}" -ne 1 ] || [ ! -f "''${native_libraries[0]}" ]; then
              printf 'expected exactly one hl-native shared library, found %s\n' "''${#native_libraries[@]}" >&2
              exit 1
            fi
            install -Dm755 "''${native_libraries[0]}" "$out/lib/$(basename "''${native_libraries[0]}")"
            cmp "''${native_libraries[0]}" "$out/lib/$(basename "''${native_libraries[0]}")"
            runHook postInstall
          '';
          postFixup =
            lib.optionalString pkgs.stdenv.isLinux ''
            strip --strip-unneeded "$out/bin/hl-engine" "$out/bin/hl-aarch64" \
              "$out/bin/hl-x86_64" "$out/lib/libhl_native_engine.so"
            for binary in "$out/bin/hl-engine" "$out/bin/hl-aarch64" "$out/bin/hl-x86_64"
            do
              existing_rpath=$(patchelf --print-rpath "$binary")
              filtered_rpath=""
              IFS=: read -ra rpath_entries <<< "$existing_rpath"
              for entry in "''${rpath_entries[@]}"; do
                if [ "$entry" != "$out/lib" ]; then
                  filtered_rpath="''${filtered_rpath:+$filtered_rpath:}$entry"
                fi
              done
              patchelf --set-rpath "\$ORIGIN/../lib''${filtered_rpath:+:$filtered_rpath}" "$binary"
              patchelf --print-needed "$binary" | grep -Fx libhl_native_engine.so >/dev/null
              test "$(patchelf --print-rpath "$binary" | cut -d: -f1)" = '$ORIGIN/../lib'
              ! patchelf --print-rpath "$binary" | tr : '\n' | grep -Fx "$out/lib" >/dev/null
            done
            ''
            + lib.optionalString pkgs.stdenv.isDarwin ''
              for binary in "$out/bin/hl-engine" "$out/bin/hl-aarch64" "$out/bin/hl-x86_64"
              do
                install_name_tool -add_rpath '@loader_path/../lib' "$binary"
              done
            '';
          meta = {
            description = "Userspace execution engine for Linux programs";
            homepage = "https://github.com/husklet/engine";
            license = lib.licenses.mit;
            platforms = systems;
            mainProgram = "hl-engine";
          };
        };

      verificationFor =
        pkgs:
        let
          alpine = if pkgs.stdenv.isLinux then linuxAlpineFor pkgs else null;
        in
        (rustPlatformFor pkgs).buildRustPackage (
          {
            pname = "hl-engine-verification";
            inherit version;
            src = workspaceSource;
            cargoLock.lockFile = ./Cargo.lock;
            strictDeps = true;
            nativeBuildInputs = commonNativeInputs pkgs ++ [
              (rustFor pkgs)
            ];
            doCheck = false;
            buildPhase = ''
              runHook preBuild

              export CARGO_BUILD_JOBS="$NIX_BUILD_CORES"
              if [ "$NIX_BUILD_CORES" -gt 256 ]; then
                export HL_COMPAT_JOBS=256
              else
                export HL_COMPAT_JOBS="$NIX_BUILD_CORES"
              fi

              cargo fmt --all --check --message-format short
              cargo run --locked --offline -q -p hl-design-lint -- --policy lint.toml src tests
              cargo run --locked --offline -q -p hl-design-lint -- --policy lint.toml --cases lint src tests
              bear --output "$TMPDIR/compile_commands.json" -- \
                cargo build -p engine -p testing --bins --locked --offline
              cargo run --locked --offline -q -p hl-design-lint -- \
                --c-analyzers "$TMPDIR" src tests
              export HL_TEST_ENGINE_APP_BIN_DIR="$PWD/target/debug"
              cargo check --workspace --all-targets --locked --offline
              cargo clippy --workspace --all-targets --locked --offline -- -D warnings
              cargo test --workspace --all-targets --locked --offline --no-fail-fast
              cargo test --workspace --doc --locked --offline
              ${lib.optionalString pkgs.stdenv.isLinux ''
                # AddressSanitizer covers native lifetime violations that leak
                # accounting cannot observe. Run the bounded ownership tests,
                # then prove that the instrumentation rejects a C heap UAF.
                export HL_C_SANITIZER=address
                cargo test -p hl-native --test executable_authority --locked --offline --no-run
                authority_tests=(target/debug/deps/executable_authority-*)
                authority_test=""
                for candidate in "''${authority_tests[@]}"; do
                  if [ -x "$candidate" ] && [ "''${candidate##*.}" != d ]; then
                    if [ -n "$authority_test" ]; then
                      printf 'multiple executable-authority test binaries found\n' >&2
                      exit 1
                    fi
                    authority_test="$candidate"
                  fi
                done
                if [ -z "$authority_test" ]; then
                  printf 'executable-authority test binary is absent\n' >&2
                  exit 1
                fi
                asan_runtime="$(${pkgs.stdenv.cc}/bin/cc -print-file-name=libasan.so)"
                ASAN_OPTIONS="detect_leaks=0:halt_on_error=1:exitcode=97:log_path=$TMPDIR/asan-clean" \
                  LD_PRELOAD="$asan_runtime" "$authority_test"
                if compgen -G "$TMPDIR/asan-clean*" >/dev/null; then
                  printf 'AddressSanitizer reported an error in the clean lifecycle tests\n' >&2
                  cat "$TMPDIR"/asan-clean* >&2
                  exit 1
                fi

                set +e
                ASAN_OPTIONS="detect_leaks=0:halt_on_error=1:exitcode=97:log_path=$TMPDIR/asan-non-vacuity" \
                  LD_PRELOAD="$asan_runtime" "$authority_test" \
                  --ignored --exact deliberate_native_use_after_free_is_visible_to_address_sanitizer
                asan_probe_status=$?
                set -e
                if [ "$asan_probe_status" -eq 0 ] ||
                   ! grep -Eq 'AddressSanitizer: heap-use-after-free' "$TMPDIR"/asan-non-vacuity*; then
                  printf 'AddressSanitizer did not reject the deliberate native UAF (exit=%s)\n' "$asan_probe_status" >&2
                  cat "$TMPDIR"/asan-non-vacuity* >&2
                  exit 1
                fi

                # Exercise bounded production-engine authority ownership under
                # LeakSanitizer. Then prove the instrumentation is live with a
                # deliberate 4,096-byte native leak.
                export HL_C_SANITIZER=leak
                export LSAN_OPTIONS="suppressions=$PWD/tests/lsan.supp:print_suppressions=1:exitcode=97:log_path=$TMPDIR/lsan-clean"
                cargo test -p hl-native --test executable_authority --locked --offline
                if compgen -G "$TMPDIR/lsan-clean*" >/dev/null; then
                  printf 'LeakSanitizer reported an error in the clean lifecycle tests\n' >&2
                  cat "$TMPDIR"/lsan-clean* >&2
                  exit 1
                fi

                export LSAN_OPTIONS="suppressions=$PWD/tests/lsan.supp:print_suppressions=1:exitcode=97:log_path=$TMPDIR/lsan-non-vacuity"
                set +e
                cargo test -p hl-native --test executable_authority --locked --offline -- \
                  --ignored --exact deliberate_native_leak_is_visible_to_memcheck
                lsan_probe_status=$?
                set -e
                if [ "$lsan_probe_status" -ne 97 ] ||
                   ! grep -Eq 'LeakSanitizer: detected memory leaks' "$TMPDIR"/lsan-non-vacuity* ||
                   ! grep -Eq '4096 byte\(s\) leaked in 1 allocation\(s\)' "$TMPDIR"/lsan-non-vacuity*; then
                  printf 'LeakSanitizer did not reject the deliberate native leak (exit=%s)\n' "$lsan_probe_status" >&2
                  cat "$TMPDIR"/lsan-non-vacuity* >&2
                  exit 1
                fi
                unset LSAN_OPTIONS

                # Memcheck is deliberately independent of compiler sanitizers. These
                # bounded authority lifecycle tests do not execute generated guest
                # code, which Valgrind cannot reliably inspect.
                export HL_C_SANITIZER=memcheck
                cargo test -p hl-native --test executable_authority --locked --offline --no-run
                authority_tests=(target/debug/deps/executable_authority-*)
                authority_test=""
                for candidate in "''${authority_tests[@]}"; do
                  if [ -x "$candidate" ] && [ "''${candidate##*.}" != d ]; then
                    if [ -n "$authority_test" ]; then
                      printf 'multiple executable-authority test binaries found\n' >&2
                      exit 1
                    fi
                    authority_test="$candidate"
                  fi
                done
                if [ -z "$authority_test" ]; then
                  printf 'executable-authority test binary is absent\n' >&2
                  exit 1
                fi
                valgrind \
                  --leak-check=full \
                  --show-leak-kinds=definite,indirect \
                  --errors-for-leak-kinds=definite,indirect \
                  --error-exitcode=97 \
                  --log-file="$TMPDIR/memcheck-clean.log" \
                  "$authority_test"
                if ! grep -q 'ERROR SUMMARY: 0 errors' "$TMPDIR/memcheck-clean.log"; then
                  printf 'Valgrind clean lifecycle verdict is absent\n' >&2
                  cat "$TMPDIR/memcheck-clean.log" >&2
                  exit 1
                fi

                set +e
                valgrind \
                  --leak-check=full \
                  --show-leak-kinds=definite,indirect \
                  --errors-for-leak-kinds=definite,indirect \
                  --error-exitcode=97 \
                  --log-file="$TMPDIR/memcheck-non-vacuity.log" \
                  "$authority_test" \
                  --ignored \
                  --exact deliberate_native_leak_is_visible_to_memcheck
                memcheck_probe_status=$?
                set -e
                if [ "$memcheck_probe_status" -ne 97 ] ||
                   ! grep -Eq 'definitely lost: 4,096 bytes in 1 blocks' "$TMPDIR/memcheck-non-vacuity.log"; then
                  printf 'Valgrind did not reject the deliberate native leak (exit=%s)\n' "$memcheck_probe_status" >&2
                  cat "$TMPDIR/memcheck-non-vacuity.log" >&2
                  exit 1
                fi
                unset HL_C_SANITIZER
              ''}
              runHook postBuild
            '';
            installPhase = ''
              mkdir -p "$out"
              touch "$out/passed"
            '';
          }
          // lib.optionalAttrs pkgs.stdenv.isLinux {
            HL_SCENARIO_TARGET = alpine.target;
            HL_ALPINE_ARCHIVE = alpine.archive;
          }
        );

      installedProductFor =
        pkgs: engine:
        pkgs.runCommand "hl-engine-installed-product"
          {
            nativeBuildInputs = [
              pkgs.binutils
              pkgs.patchelf
            ];
          }
          ''
            set -euo pipefail
            prefix="$TMPDIR/installed-product"
            mkdir -p "$prefix"
            cp -a ${engine}/. "$prefix"/

            library="$prefix/lib/libhl_native_engine.so"
            for artifact in "$prefix"/bin/* "$prefix"/lib/*; do
              test "$(stat -c %a "$artifact")" = 555
              ! readelf -S "$artifact" | grep -E '\.(debug|symtab)' >/dev/null
              ! strings "$artifact" | grep -E '/build/|/cargo-vendor-dir/' >/dev/null
            done
            readelf -d "$library" |
              grep -F 'Library soname: [libhl_native_engine.so]' >/dev/null
            readelf --dyn-syms --wide "$library" |
              awk '$4 == "FUNC" && $5 == "GLOBAL" && $6 == "DEFAULT" && $7 != "UND" { print $8 }' |
              sed 's/@.*//' | sort -u > "$TMPDIR/actual-exports"
            cp src/runtime/hl-native/src/native/bridge/exports.txt "$TMPDIR/expected-exports"
            diff -u "$TMPDIR/expected-exports" "$TMPDIR/actual-exports"

            for name in hl-engine hl-aarch64 hl-x86_64
            do
              binary="$prefix/bin/$name"
              test -x "$binary"
              patchelf --print-needed "$binary" | grep -Fx libhl_native_engine.so >/dev/null
              patchelf --print-rpath "$binary" | tr : '\n' > "$TMPDIR/$name.runpath"
              test "$(head -n1 "$TMPDIR/$name.runpath")" = '$ORIGIN/../lib'
              tail -n+2 "$TMPDIR/$name.runpath" | while IFS= read -r entry; do
                case "$entry" in
                  /nix/store/*/lib) ;;
                  *) printf 'unsafe RUNPATH entry in %s: %s\n' "$name" "$entry" >&2; exit 1 ;;
                esac
              done
              env -i PATH=/usr/bin:/bin HOME="$prefix/home" \
                "$binary" --backend-receipt > "$TMPDIR/$name.receipt"
              env -i PATH=/usr/bin:/bin HOME="$prefix/home" \
                "$binary" --backend-receipt > "$TMPDIR/$name.receipt-repeat"
              cmp "$TMPDIR/$name.receipt" "$TMPDIR/$name.receipt-repeat"
              grep -F '"backend":"retained-c"' "$TMPDIR/$name.receipt" >/dev/null
              expected_hash=$(sha256sum "$binary" | cut -d' ' -f1)
              grep -F "\"engine_sha256\":\"$expected_hash\"" \
                "$TMPDIR/$name.receipt" >/dev/null
            done

            LD_DEBUG=libs "$prefix/bin/hl-engine" --backend-receipt \
              > "$TMPDIR/receipt.json" 2> "$TMPDIR/loader.log"
            grep -F "trying file=$prefix/bin/../lib/libhl_native_engine.so" \
              "$TMPDIR/loader.log" >/dev/null
            grep -F "calling init: $prefix/bin/../lib/libhl_native_engine.so" \
              "$TMPDIR/loader.log" >/dev/null

            chmod u+w "$prefix/lib"
            mv "$library" "$TMPDIR/libhl_native_engine.so"
            if env -i PATH=/usr/bin:/bin HOME="$prefix/home" \
              "$prefix/bin/hl-engine" --backend-receipt \
              > "$TMPDIR/missing-library.stdout" 2> "$TMPDIR/missing-library.stderr"; then
              printf '%s\n' 'engine started without its packaged sibling native library' >&2
              exit 1
            fi
            test ! -s "$TMPDIR/missing-library.stdout"
            grep -F 'libhl_native_engine.so' "$TMPDIR/missing-library.stderr" >/dev/null
            mv "$TMPDIR/libhl_native_engine.so" "$library"

            mkdir -p "$out"
            cp "$TMPDIR/receipt.json" "$out/backend-receipt.json"
            (cd "$prefix" && sha256sum bin/hl-engine bin/hl-aarch64 bin/hl-x86_64 \
              lib/libhl_native_engine.so) > "$out/SHA256SUMS"
            printf '%s\n' \
              'copied-prefix bounded RUNPATH, NEEDED, backend ABI, and sibling-library loader selection passed' \
              > "$out/evidence"
          '';

      linuxHostCompileFor =
        pkgs: architecture:
        let
          native =
            pkgs.stdenv.hostPlatform.isLinux && pkgs.stdenv.hostPlatform.parsed.cpu.name == architecture;
          crossAttr = if architecture == "aarch64" then "aarch64-multiplatform" else "gnu64";
          targetPkgs = if native then pkgs else pkgs.pkgsCross.${crossAttr};
          compiler =
            if native then
              "${pkgs.stdenv.cc}/bin/cc"
            else
              "${targetPkgs.stdenv.cc}/bin/${targetPkgs.stdenv.cc.targetPrefix}cc";
          cxx =
            if native then
              "${pkgs.stdenv.cc}/bin/c++"
            else
              "${targetPkgs.stdenv.cc}/bin/${targetPkgs.stdenv.cc.targetPrefix}c++";
          target = "${architecture}-unknown-linux-gnu";
          targetKey = lib.toUpper (lib.replaceStrings [ "-" ] [ "_" ] target);
          fileArchitecture = if architecture == "aarch64" then "ARM aarch64" else "x86-64";
        in
        (rustPlatformFor pkgs).buildRustPackage {
          pname = "hl-native-${architecture}-linux-compile";
          inherit version;
          src = workspaceSource;
          cargoLock.lockFile = ./Cargo.lock;
          strictDeps = true;
          nativeBuildInputs = [
            (rustFor pkgs)
            targetPkgs.stdenv.cc
            pkgs.file
          ];
          doCheck = false;
          buildPhase = ''
            runHook preBuild
            export CC_${lib.replaceStrings [ "-" ] [ "_" ] target}=${lib.escapeShellArg compiler}
            export CARGO_TARGET_${targetKey}_LINKER=${lib.escapeShellArg compiler}
            cargo build --locked --offline --target ${target} -p hl-native -p hl-engine
            test -f target/${target}/debug/libhl_native.rlib
            native_libraries=(target/${target}/debug/build/hl-native-*/out/libhl_native_engine.so)
            test "''${#native_libraries[@]}" -eq 1
            test -s "''${native_libraries[0]}"
            ${targetPkgs.stdenv.cc.targetPrefix}readelf -d "''${native_libraries[0]}" \
              | grep -F 'Library soname: [libhl_native_engine.so]' >/dev/null
            ${targetPkgs.stdenv.cc.targetPrefix}readelf --dyn-syms --wide "''${native_libraries[0]}" \
              | awk '$4 == "FUNC" && $5 == "GLOBAL" && $6 == "DEFAULT" && $7 != "UND" { print $8 }' \
              | sed 's/@.*//' | sort -u > "$TMPDIR/actual-exports"
            cp src/runtime/hl-native/src/native/bridge/exports.txt "$TMPDIR/expected-exports"
            diff -u "$TMPDIR/expected-exports" "$TMPDIR/actual-exports"
            native_directory="$(dirname "''${native_libraries[0]}")"
            ${lib.escapeShellArg compiler} -std=c11 -Wall -Wextra -Werror \
              -Isrc/runtime/hl-native/src/native/include \
              tests/native/host-abi/unix.c -L"$native_directory" \
              -Wl,-rpath-link,"$native_directory" -lhl_native_engine -o public-abi-c
            ${lib.escapeShellArg cxx} -std=c++20 -Wall -Wextra -Werror \
              -Isrc/runtime/hl-native/src/native/include \
              tests/native/host-abi/unix.cpp -L"$native_directory" \
              -Wl,-rpath-link,"$native_directory" -lhl_native_engine -o public-abi-cxx
            file public-abi-c public-abi-cxx | grep -F ${lib.escapeShellArg fileArchitecture}
            ${targetPkgs.stdenv.cc.targetPrefix}readelf -d public-abi-c \
              | grep -F 'Shared library: [libhl_native_engine.so]' >/dev/null
            ${targetPkgs.stdenv.cc.targetPrefix}readelf -d public-abi-cxx \
              | grep -F 'Shared library: [libhl_native_engine.so]' >/dev/null
            runHook postBuild
          '';
          installPhase = ''
            mkdir -p "$out"
            printf '%s\n' ${lib.escapeShellArg "full Cargo/C shared-engine compile, exact SONAME/export parity, and strict C/C++ LP64 public-ABI link contracts for ${target}"} > "$out/evidence"
          '';
        };

      nativeScanBuildFor =
        pkgs: architecture:
        let
          native =
            pkgs.stdenv.hostPlatform.isLinux && pkgs.stdenv.hostPlatform.parsed.cpu.name == architecture;
          crossAttr = if architecture == "aarch64" then "aarch64-multiplatform" else "gnu64";
          targetPkgs = if native then pkgs else pkgs.pkgsCross.${crossAttr};
          compiler =
            if native then
              "${pkgs.stdenv.cc}/bin/cc"
            else
              "${targetPkgs.stdenv.cc}/bin/${targetPkgs.stdenv.cc.targetPrefix}cc";
          portableWarnings = [
              "-Werror=implicit-function-declaration"
              "-Werror=incompatible-pointer-types"
              "-Werror=int-conversion"
              "-Werror=return-type"
              "-Werror=type-limits"
              "-Werror=null-dereference"
              "-Werror=uninitialized"
              "-Werror=switch-default"
            ];
          strictWarnings =
            portableWarnings
            ++ lib.optionals targetPkgs.stdenv.cc.isGNU [
              "-Werror=maybe-uninitialized"
              "-Werror=restrict"
              "-Werror=logical-op"
              "-Werror=duplicated-branches"
              "-Werror=stack-usage=262144"
            ];
        in
        pkgs.runCommand "hl-native-${architecture}-scan-build"
          {
            src = workspaceSource;
            nativeBuildInputs = [
              pkgs.clang-analyzer
              pkgs.coreutils
              targetPkgs.stdenv.cc
            ];
          }
          ''
            cp -R "$src" source
            chmod -R u+w source
            cd source
            mkdir reports
            if printf '%s\n' 'int warning_probe(unsigned value) { return value < 0; }' \
              | ${lib.escapeShellArg compiler} -x c -fsyntax-only -Werror=type-limits -
            then
              printf '%s\n' 'strict C warning probe unexpectedly compiled' >&2
              exit 1
            fi
            ${lib.escapeShellArg compiler} \
              -O2 -fPIC -g -fno-omit-frame-pointer -std=c11 \
              -Isrc/runtime/hl-native/src/native \
              -Isrc/runtime/hl-native/src/native/include \
              -fvisibility=hidden \
              ${lib.escapeShellArgs strictWarnings} \
              -DHL_SHARED -DHL_BUILDING_ENGINE -DHL_ENABLE_LOGGING=0 \
              -DHL_TRANSLIT_DEFAULT=0 -D_GNU_SOURCE -DHL_EMBEDDED_BUILD=1 \
              -DHL_ENGINE_NO_MAIN=1 -DHL_ENGINE_NO_STANDALONE=1 \
              -DHL_TARGET_NAMESPACE=${architecture} \
              -fsyntax-only src/runtime/hl-native/src/native/engine/target/${architecture}.c
            timeout 10m scan-build --status-bugs -o reports \
              ${lib.escapeShellArg compiler} \
              -O2 -fPIC -g -fno-omit-frame-pointer -std=c11 \
              -Isrc/runtime/hl-native/src/native \
              -Isrc/runtime/hl-native/src/native/include \
              -fvisibility=hidden \
              ${lib.escapeShellArgs portableWarnings} \
              -DHL_SHARED -DHL_BUILDING_ENGINE -DHL_ENABLE_LOGGING=0 \
              -DHL_TRANSLIT_DEFAULT=0 -D_GNU_SOURCE -DHL_EMBEDDED_BUILD=1 \
              -DHL_ENGINE_NO_MAIN=1 -DHL_ENGINE_NO_STANDALONE=1 \
              -DHL_TARGET_NAMESPACE=${architecture} \
              -c src/runtime/hl-native/src/native/engine/target/${architecture}.c \
              -o engine.o
            mkdir -p "$out"
            printf '%s\n' \
              ${lib.escapeShellArg "scan-build --status-bugs and strict C declaration/type/range/return diagnostics passed for the ${architecture} Linux unity translation unit"} \
              > "$out/evidence"
          '';

      windowsGnuSmokeFor =
        pkgs:
        let
          target = "x86_64-pc-windows-gnu";
          targetKey = "X86_64_PC_WINDOWS_GNU";
          windows = pkgs.pkgsCross.mingwW64;
          compiler = "${windows.stdenv.cc}/bin/${windows.stdenv.cc.targetPrefix}cc";
          cxx = "${windows.stdenv.cc}/bin/${windows.stdenv.cc.targetPrefix}c++";
        in
        (rustPlatformFor pkgs).buildRustPackage {
          pname = "hl-native-windows-gnu-smoke";
          inherit version;
          src = workspaceSource;
          cargoLock.lockFile = ./Cargo.lock;
          strictDeps = true;
          nativeBuildInputs = [
            (rustFor pkgs)
            windows.stdenv.cc
            pkgs.file
          ];
          buildInputs = [ windows.windows.mcfgthreads ];
          doCheck = false;
          buildPhase = ''
            runHook preBuild
            export CC_x86_64_pc_windows_gnu=${lib.escapeShellArg compiler}
            export CARGO_TARGET_${targetKey}_LINKER=${lib.escapeShellArg compiler}
            export HL_NATIVE_COMPILE_CHECK=1
            cargo check --locked --offline --target ${target} -p hl-native -p hl-engine 2>&1 |
              tee "$TMPDIR/windows-contract.log"
            dll="$(find target/${target}/debug/build -path '*/out/hl_native_engine.dll' -print -quit)"
            import="$(find target/${target}/debug/build -path '*/out/libhl_native_engine.dll.a' -print -quit)"
            test -n "$dll"
            test -n "$import"
            ${windows.stdenv.cc.targetPrefix}objdump -f "$dll" \
              | grep -F 'file format pei-x86-64' >/dev/null
            file "$dll" | grep -E 'PE32\+.*DLL.*x86-64'
            file "$import" | grep -F 'current ar archive'
            cp src/runtime/hl-native/src/native/bridge/exports.txt expected-engine-exports
            ${windows.stdenv.cc.targetPrefix}nm -g "$import" \
              | awk '$2 == "T" && $3 ~ /^hl_/ { print $3 }' \
              | sort -u > actual-engine-exports
            diff -u expected-engine-exports actual-engine-exports
            ${lib.escapeShellArg compiler} -std=c11 -DHL_SHARED -DHL_BUILDING_ENGINE \
              -Isrc/runtime/hl-native/src/native -Isrc/runtime/hl-native/src/native/include \
              -c src/runtime/hl-native/src/native/bridge/host.c -o host-bridge.obj
            ${windows.stdenv.cc.targetPrefix}objdump -f host-bridge.obj \
              | grep -F 'file format pe-x86-64' >/dev/null
            ${windows.stdenv.cc.targetPrefix}objdump -f host-bridge.obj \
              | grep -F 'architecture: i386:x86-64' >/dev/null
            mkdir windows-host-objects
            for source in src/runtime/hl-native/src/native/host/windows/*.c; do
              object="windows-host-objects/$(basename "''${source%.c}").obj"
              ${lib.escapeShellArg compiler} -std=c11 -DHL_SHARED -DHL_BUILDING_ENGINE \
                -Isrc/runtime/hl-native/src/native \
                -Isrc/runtime/hl-native/src/native/include \
                -Isrc/runtime/hl-native/src/native/toolchain/msvc-posix/include \
                -c "$source" -o "$object"
              ${windows.stdenv.cc.targetPrefix}objdump -f "$object" \
                | grep -F 'file format pe-x86-64' >/dev/null
            done
            ${windows.stdenv.cc.targetPrefix}ld -r windows-host-objects/*.obj \
              -o windows-host-services.obj
            ${windows.stdenv.cc.targetPrefix}objdump -f windows-host-services.obj \
              | grep -F 'architecture: i386:x86-64' >/dev/null
            ${lib.escapeShellArg compiler} -std=c11 -DHL_SHARED -DHL_BUILDING_ENGINE \
              -Isrc/runtime/hl-native/src/native \
              -Isrc/runtime/hl-native/src/native/include \
              -Isrc/runtime/hl-native/src/native/toolchain/msvc-posix/include \
              -include src/runtime/hl-native/src/native/toolchain/msvc-posix/include/prelude.h \
              -c src/runtime/hl-native/src/native/toolchain/msvc-posix/compatibility.c \
              -o windows-posix-compatibility.obj
            ${windows.stdenv.cc.targetPrefix}objdump -f windows-posix-compatibility.obj \
              | grep -F 'architecture: i386:x86-64' >/dev/null
            ${lib.escapeShellArg compiler} -std=c11 -Wall -Wextra -Werror \
              -DHL_SHARED -DHL_ABI_COMPILE_CONTRACT \
              -Isrc/runtime/hl-native/src/native/include \
              -c tests/native/host-abi/windows.c -o public-abi-c.obj
            ${windows.stdenv.cc.targetPrefix}objdump -f public-abi-c.obj \
              | grep -F 'file format pe-x86-64' >/dev/null
            ${windows.stdenv.cc.targetPrefix}objdump -f public-abi-c.obj \
              | grep -F 'architecture: i386:x86-64' >/dev/null
            ${lib.escapeShellArg cxx} -std=c++20 -Wall -Wextra -Werror \
              -DHL_SHARED -Isrc/runtime/hl-native/src/native/include \
              -c tests/native/host-abi/windows.cpp -o public-abi-cxx.obj
            ${windows.stdenv.cc.targetPrefix}objdump -f public-abi-cxx.obj \
              | grep -F 'file format pe-x86-64' >/dev/null
            ${windows.stdenv.cc.targetPrefix}objdump -f public-abi-cxx.obj \
              | grep -F 'architecture: i386:x86-64' >/dev/null
            ${lib.escapeShellArg compiler} -std=c11 -DHL_SHARED -DHL_BUILDING_ENGINE \
              -DHL_ABI_FIXTURE_EXPORT \
              -Isrc/runtime/hl-native/src/native/include \
              -L${windows.windows.mcfgthreads}/lib \
              -shared tests/native/host-abi/windows.c -o hl-abi-fixture.dll \
              -Wl,--out-implib,libhl-abi-fixture.dll.a
            ${windows.stdenv.cc.targetPrefix}objdump -f hl-abi-fixture.dll \
              | grep -F 'file format pei-x86-64' >/dev/null
            ${windows.stdenv.cc.targetPrefix}objdump -f hl-abi-fixture.dll \
              | grep -F 'architecture: i386:x86-64' >/dev/null
            file hl-abi-fixture.dll | grep -E 'PE32\+.*DLL.*x86-64'
            file libhl-abi-fixture.dll.a | grep -F 'current ar archive'
            ${windows.stdenv.cc.targetPrefix}nm -g libhl-abi-fixture.dll.a \
              | grep -F ' T hl_ci_engine_abi' >/dev/null
            runHook postBuild
          '';
          installPhase = ''
            mkdir -p "$out"
            printf '%s\n' \
              'GNU Windows hl-native/hl-engine Rust target compile, complete engine DLL/import-library link with exact public exports, every Windows host-service translation unit, forced POSIX compatibility, and strict C/C++ public-header contracts; this is compile/link evidence, not MSVC SDK or runtime proof' \
              > "$out/evidence"
          '';
        };

      darwinInstalledProductFor =
        pkgs: engine:
        pkgs.runCommand "hl-engine-darwin-installed-product"
          { nativeBuildInputs = [ pkgs.darwin.cctools ]; }
          ''
          set -euo pipefail
          prefix="$TMPDIR/installed-product"
          mkdir -p "$prefix"
          cp -a ${engine}/. "$prefix"/
          library="$prefix/lib/libhl_native_engine.dylib"
          test -s "$library"
          lipo -archs "$library" | grep -Fx 'arm64' >/dev/null
          otool -D "$library" | grep -Fx '@rpath/libhl_native_engine.dylib' >/dev/null
          nm -gjU "$library" | sort -u > "$TMPDIR/actual-exports"
          sed 's/^/_/' ${workspaceSource}/src/runtime/hl-native/src/native/bridge/exports.txt \
            > "$TMPDIR/expected-exports"
          diff -u "$TMPDIR/expected-exports" "$TMPDIR/actual-exports"
          for name in hl-engine hl-aarch64 hl-x86_64; do
            binary="$prefix/bin/$name"
            test -x "$binary"
            lipo -archs "$binary" | grep -Fx 'arm64' >/dev/null
            otool -L "$binary" | grep -F '@rpath/libhl_native_engine.dylib' >/dev/null
            otool -l "$binary" | grep -A2 LC_RPATH | grep -F '@loader_path/../lib' >/dev/null
            env -i PATH=/usr/bin:/bin HOME="$prefix/home" \
              "$binary" --backend-receipt > "$TMPDIR/$name.receipt"
            env -i PATH=/usr/bin:/bin HOME="$prefix/home" \
              "$binary" --backend-receipt > "$TMPDIR/$name.receipt-repeat"
            cmp "$TMPDIR/$name.receipt" "$TMPDIR/$name.receipt-repeat"
            grep -F '"backend":"retained-c"' "$TMPDIR/$name.receipt" >/dev/null
            expected_hash=$(sha256sum "$binary" | cut -d' ' -f1)
            grep -F "\"engine_sha256\":\"$expected_hash\"" "$TMPDIR/$name.receipt" >/dev/null
          done

          chmod u+w "$prefix/lib"
          mv "$library" "$TMPDIR/libhl_native_engine.dylib"
          if env -i PATH=/usr/bin:/bin HOME="$prefix/home" \
            "$prefix/bin/hl-engine" --backend-receipt \
            > "$TMPDIR/missing-library.stdout" 2> "$TMPDIR/missing-library.stderr"; then
            printf '%s\n' 'engine started without its packaged sibling native library' >&2
            exit 1
          fi
          test ! -s "$TMPDIR/missing-library.stdout"
          grep -F 'libhl_native_engine.dylib' "$TMPDIR/missing-library.stderr" >/dev/null
          mv "$TMPDIR/libhl_native_engine.dylib" "$library"
          mkdir -p "$out"
          printf '%s\n' 'native Darwin copied-prefix exact ARM64 architecture, install name, exports, rpath, deterministic hash-bound backend receipts, and sibling-library isolation passed' > "$out/evidence"
          '';

      darwinHostAbiFor =
        pkgs: engine:
        pkgs.runCommand "hl-native-darwin-host-abi"
          {
            nativeBuildInputs = [ pkgs.stdenv.cc ];
          }
          ''
            set -euo pipefail
            mkdir -p "$TMPDIR/product"
            cp ${engine}/lib/libhl_native_engine.dylib "$TMPDIR/product/"
            library="$TMPDIR/product/libhl_native_engine.dylib"
            lipo -archs "$library" | grep -Fx arm64 >/dev/null

            ${pkgs.stdenv.cc}/bin/cc -std=c11 -Wall -Wextra -Werror \
              -DHL_SHARED -I${workspaceSource}/src/runtime/hl-native/src/native/include \
              ${workspaceSource}/tests/native/host-abi/unix.c \
              -L"$TMPDIR/product" -lhl_native_engine \
              -Wl,-rpath,@loader_path -o "$TMPDIR/product/public-abi-c"
            ${pkgs.stdenv.cc}/bin/c++ -std=c++20 -Wall -Wextra -Werror \
              -DHL_SHARED -I${workspaceSource}/src/runtime/hl-native/src/native/include \
              ${workspaceSource}/tests/native/host-abi/unix.cpp \
              -L"$TMPDIR/product" -lhl_native_engine \
              -Wl,-rpath,@loader_path -o "$TMPDIR/product/public-abi-cxx"

            for binary in "$TMPDIR/product/public-abi-c" "$TMPDIR/product/public-abi-cxx"; do
              lipo -archs "$binary" | grep -Fx arm64 >/dev/null
              otool -L "$binary" | grep -F '@rpath/libhl_native_engine.dylib' >/dev/null
              otool -l "$binary" | grep -A2 LC_RPATH | grep -F '@loader_path' >/dev/null
              env -i PATH=/usr/bin:/bin HOME="$TMPDIR/home" "$binary"
            done

            mkdir -p "$out"
            printf '%s\n' \
              'native Darwin ARM64 strict C/C++ public-ABI compile, dylib link, loader-path resolution, and execution passed' \
              > "$out/evidence"
          '';

      darwinCrossContractFor =
        pkgs:
        pkgs.runCommand "hl-native-darwin-cross-contract" { } ''
          mkdir -p "$out"
          if [ -n "''${HL_APPLE_SDK:-}" ]; then
            printf '%s\n' 'FAIL: impure Apple SDK injection is forbidden in a Nix check; package the SDK explicitly' >&2
            exit 1
          fi
          printf '%s\n' \
            'SKIP: Linux cannot honestly compile the Darwin host backend without a packaged Apple SDK; aarch64-darwin runs the native flake verification check' \
            > "$out/evidence"
        '';
    in
    {
      packages = forAllSystems (
        pkgs:
        let
          engine = packageFor pkgs;
        in
        {
          inherit engine;
          default = if pkgs.stdenv.isDarwin then macDevFor pkgs else engine;
        }
        // lib.optionalAttrs pkgs.stdenv.isDarwin {
          mac-base = macBaseFor pkgs;
          mac-dev = macDevFor pkgs;
        }
      );

      checks = forAllSystems (
        pkgs:
        let
          verification = verificationFor pkgs;
          engine = packageFor pkgs;
        in
        {
          package = verification;
          workspace = verification;
          test = verification;
          "design-lint" = verification;
          "lint-cases" = verification;
          "compat-fixtures" = verification;
        }
        // lib.optionalAttrs pkgs.stdenv.isLinux {
          "installed-product" = installedProductFor pkgs engine;
          "host-linux-aarch64" = linuxHostCompileFor pkgs "aarch64";
          "host-linux-x86_64" = linuxHostCompileFor pkgs "x86_64";
          "scan-build-linux-aarch64" = nativeScanBuildFor pkgs "aarch64";
          "scan-build-linux-x86_64" = nativeScanBuildFor pkgs "x86_64";
          "host-windows-x86_64-gnu-smoke" = windowsGnuSmokeFor pkgs;
          "host-darwin-cross-contract" = darwinCrossContractFor pkgs;
        }
        // lib.optionalAttrs pkgs.stdenv.isDarwin {
          "host-darwin-aarch64-native" = darwinHostAbiFor pkgs engine;
          "installed-product" = darwinInstalledProductFor pkgs engine;
        }
      );

      apps = forAllSystems (pkgs: {
        lint-cases = {
          type = "app";
          program = "${lintCasesFor pkgs}/bin/husklet-lint-cases";
        };
      });

      devShells = forAllSystems (
        pkgs:
        let
          toolchain = toolchainFor pkgs;
          alpineArchives = alpineArchivesFor pkgs;
          alpine = if pkgs.stdenv.isLinux then linuxAlpineFor pkgs else null;
        in
        {
          default = pkgs.mkShell (
            toolchain.env
            // {
              packages = [
                pkgs.clang-tools
                pkgs.cppcheck
                pkgs.go
                pkgs.nixfmt
                pkgs.pkg-config
                (rustFor pkgs)
              ]
              ++ lib.optionals toolchain.canBuildGuests (
                toolchain.crossCompilers
                ++ toolchain.rustStaticLinkers
                ++ toolchain.compilerAliases
                ++ toolchain.emulators
              );
              shellHook = ''
                export CC="${toolchain.env.CC}"
                export NATIVE_CC="${toolchain.env.NATIVE_CC}"
                export CARGO_BUILD_JOBS="''${CARGO_BUILD_JOBS:-1}"
                export HL_COMPAT_JOBS="''${HL_COMPAT_JOBS:-1}"
              '';
            }
            // lib.optionalAttrs pkgs.stdenv.isLinux {
              HL_SCENARIO_TARGET = alpine.target;
              HL_ALPINE_ARCHIVE = alpine.archive;
              # The signed application is macOS-only, but its GTK4/VTE sources are the largest body of
              # code in the tree and `required-features = ["gui"]` makes Cargo skip them in silence when
              # the toolkit is absent. Carrying the same libraries on Linux lets the explicit GUI Clippy
              # invocation type-check them, so an engine refactor cannot redden the app behind a green gate.
              nativeBuildInputs = [
                pkgs.gobject-introspection
                pkgs.glib
                pkgs.gdk-pixbuf
              ];
              buildInputs = [
                pkgs.gtk4
                pkgs.librsvg
                pkgs.vte-gtk4
              ];
            }
            // lib.optionalAttrs pkgs.stdenv.isDarwin {
              nativeBuildInputs = [
                pkgs.gobject-introspection
                pkgs.glib
                pkgs.gdk-pixbuf
                pkgs.macdylibbundler
                pkgs.create-dmg
              ];
              buildInputs = [
                pkgs.gtk4
                pkgs.librsvg
                pkgs.vte-gtk4
              ];
              HL_GTK4 = pkgs.gtk4;
              HL_LIBRSVG = pkgs.librsvg;
              HL_GDK_PIXBUF = pkgs.gdk-pixbuf;
              HL_ADWAITA_ICONS = pkgs.adwaita-icon-theme;
              HL_HICOLOR_ICONS = pkgs.hicolor-icon-theme;
              HL_GSETTINGS_SCHEMAS = pkgs.gsettings-desktop-schemas;
              HL_ALPINE_ARCHIVE = alpineArchives.arm64;
            }
          );
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
