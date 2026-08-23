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
          nativeCXX = "${pkgs.stdenv.cc}/bin/c++";
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
          # The guest sqlite the compatibility guests link against cannot be cross-built from a Darwin
          # builder: its `tcl` dependency's configure misdetects the build host and compiles
          # `tclUnixTime.c` against `mach/mach_time.h`, which no Linux sysroot carries. So the alias
          # forwards sqlite's include and library paths only where sqlite exists; every guest that
          # does not link it -- `hl-native`'s `engine::tests` re-exec pair among them -- builds on
          # both hosts through the same `<isa>-linux-gnu-gcc` spelling.
          compilerAliasFor =
            guest:
            let
              guestPkgs = pkgsFor guest;
              sqliteFlags = lib.optionals pkgs.stdenv.isLinux [
                "-isystem ${lib.escapeShellArg "${guestPkgs.sqlite.dev}/include"}"
                "-L${lib.escapeShellArg "${guestPkgs.sqlite.out}/lib"}"
              ];
            in
            pkgs.writeShellScriptBin "${guest.isa}-linux-gnu-gcc" ''
              exec ${lib.escapeShellArg (ccFor guest)} \
                -L${lib.escapeShellArg "${guestPkgs.glibc.static}/lib"} \
                ${lib.concatStringsSep " " sqliteFlags} "$@"
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
          guestLibraries = lib.concatMap (guest: [
            (pkgsFor guest).sqlite.dev
            (pkgsFor guest).sqlite.out
          ]) guestISAs;
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
                CXX = nativeCXX;
                NATIVE_CC = nativeCC;
                NATIVE_CXX = nativeCXX;
              }
              guestISAs;
        };

      workspaceSource = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.gitTracked ./.;
      };

      documentationSourcePaths =
        (builtins.fromTOML (builtins.readFile ./lint.toml)).documentation.allowed;

      workspaceSourceContractFor =
        pkgs:
        pkgs.runCommand "husklet-workspace-source-contract" { } ''
          ${lib.concatMapStringsSep "\n" (
            path: "test -f ${workspaceSource}/${lib.escapeShellArg path}"
          ) documentationSourcePaths}
          touch "$out"
        '';

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
                ! patchelf --print-needed "$binary" | grep -Fx libhl_native_engine.so >/dev/null
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
          postFixup = lib.optionalString pkgs.stdenv.isLinux ''
            strip --strip-unneeded "$out/bin/hl-engine" "$out/bin/hl-aarch64" \
              "$out/bin/hl-x86_64" "$out/lib/libhl_native_engine.so"
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
          toolchain = toolchainFor pkgs;
        in
        (rustPlatformFor pkgs).buildRustPackage (
          {
            pname = "hl-engine-verification";
            inherit version;
            src = workspaceSource;
            cargoLock.lockFile = ./Cargo.lock;
            strictDeps = true;
            # `cargo check/clippy/test --workspace --all-targets` reaches `hl-gui-gtk` and
            # `storybook`, which resolve gtk4, vte-gtk4 and librsvg through pkg-config. This
            # derivation carried `buildInputs: ""`, so all three died in `gdk4-sys`'s build
            # script with "The system library `gtk4` ... was not found" and `nix flake check`
            # could not succeed on any system. The dev shell has carried the same six inputs on
            # Linux and Darwin alike since bdaf05de4; keep the two lists identical, because the
            # shell is where every lane runs this gate by hand.
            nativeBuildInputs = commonNativeInputs pkgs ++ [
              (rustFor pkgs)
              pkgs.gobject-introspection
              pkgs.glib
              pkgs.gdk-pixbuf
              pkgs.cacert
              pkgs.coreutils
              pkgs.procps
            ]
            ++ lib.optionals toolchain.canBuildGuests toolchain.compilerAliases
            ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.xorg-server pkgs.xvfb-run ];
            buildInputs = [
              pkgs.gtk4
              pkgs.librsvg
              pkgs.vte-gtk4
            ];
            doCheck = false;
            buildPhase = ''
              runHook preBuild

              export CARGO_BUILD_JOBS="$NIX_BUILD_CORES"
              export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
              # A unique root prevents two sandbox UIDs from sharing the corpus
              # runner's durable default namespace in /var/tmp.
              export TMPDIR="$(mktemp -d /tmp/husklet-verification.XXXXXX)"
              export HL_RUNTIME_WORK_ROOT="$TMPDIR/runtime"
              export HOME="$TMPDIR/home"
              export XDG_CACHE_HOME="$TMPDIR/cache"
              mkdir -p "$HOME" "$XDG_CACHE_HOME"
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
              ${lib.optionalString pkgs.stdenv.isLinux ''
                xvfb-run -a -s '-screen 0 1600x1000x24' -- \
                  cargo test --workspace --all-targets --locked --offline --no-fail-fast
              ''}
              ${lib.optionalString pkgs.stdenv.isDarwin ''
                cargo test --workspace --all-targets --locked --offline --no-fail-fast
              ''}
              ${lib.optionalString pkgs.stdenv.isLinux ''export HL_PRODUCT_CHECKPOINT_REQUIRED=1''}
              cargo test -p husklet --features runtime --lib --locked --offline --no-fail-fast
              cargo test --workspace --doc --locked --offline
              ${lib.optionalString pkgs.stdenv.isLinux ''
                build_authority_test() {
                  local receipt="$1"
                  cargo test -p hl-native --test executable_authority --locked --offline --no-run \
                    --message-format=json | tee "$receipt"
                  mapfile -t authority_tests < <(
                    sed -n 's/.*"executable":"\([^"]*\/executable_authority-[^"]*\)".*/\1/p' "$receipt"
                  )
                  if [ "''${#authority_tests[@]}" -ne 1 ] || [ ! -x "''${authority_tests[0]}" ]; then
                    printf 'expected one executable-authority artifact from this build, found %s\n' \
                      "''${#authority_tests[@]}" >&2
                    exit 1
                  fi
                  authority_test="''${authority_tests[0]}"
                }

                reject_sanitizer_reports() {
                  local label="$1"
                  local prefix="$2"
                  local found=0
                  for report in "$TMPDIR/$prefix" "$TMPDIR/$prefix".*; do
                    [ -f "$report" ] || continue
                    if [ "$found" -eq 0 ]; then
                      printf '%s reported an error in the clean lifecycle tests\n' "$label" >&2
                    fi
                    cat "$report" >&2
                    found=1
                  done
                  [ "$found" -eq 0 ]
                }

                # Prove the shell running this derivation can discover a report. The previous `compgen`
                # probe was unavailable here and its failure made the surrounding `if` silently pass.
                printf 'sanitizer-report-probe\n' > "$TMPDIR/sanitizer-probe.known"
                if reject_sanitizer_reports Probe sanitizer-probe >/dev/null 2>&1; then
                  printf 'sanitizer report probe was not discovered\n' >&2
                  exit 1
                fi
                rm "$TMPDIR/sanitizer-probe.known"

                # AddressSanitizer covers native lifetime violations that leak
                # accounting cannot observe. Run the bounded ownership tests,
                # then prove that the instrumentation rejects a C heap UAF.
                export HL_C_SANITIZER=address
                build_authority_test "$TMPDIR/asan-authority-build.json"
                asan_runtime="$(${pkgs.stdenv.cc}/bin/cc -print-file-name=libasan.so)"
                ASAN_OPTIONS="detect_leaks=0:halt_on_error=1:exitcode=97:log_path=$TMPDIR/asan-clean" \
                  LD_PRELOAD="$asan_runtime" "$authority_test"
                if ! reject_sanitizer_reports AddressSanitizer asan-clean; then
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
                if ! reject_sanitizer_reports LeakSanitizer lsan-clean; then
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
                build_authority_test "$TMPDIR/memcheck-authority-build.json"
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

      nativeHookVerificationFor =
        pkgs:
        (rustPlatformFor pkgs).buildRustPackage {
          pname = "hl-native-test-hooks-verification";
          inherit version;
          src = workspaceSource;
          cargoLock.lockFile = ./Cargo.lock;
          strictDeps = true;
          nativeBuildInputs = commonNativeInputs pkgs ++ [
            pkgs.coreutils
            (rustFor pkgs)
          ];
          doCheck = false;
          buildPhase = ''
            runHook preBuild
            export CARGO_BUILD_JOBS="$NIX_BUILD_CORES"
            for native_test in \
              bound_vector_io \
              errno_namespace \
              identity_registry \
              x86_store_preflight \
              restore_collision
            do
              timeout --kill-after=30s 10m \
                cargo test -p hl-native --features native-test-hooks --test "$native_test" \
                --locked --offline -- --test-threads=1
            done
            runHook postBuild
          '';
          installPhase = ''
            mkdir -p "$out"
            touch "$out/passed"
          '';
        };

      alpineCompatibilityFor =
        pkgs:
        let
          archives = alpineArchivesFor pkgs;
          toolchain = toolchainFor pkgs;
          arm64Compiler = builtins.elemAt toolchain.compilerAliases 0;
        in
        (rustPlatformFor pkgs).buildRustPackage {
          pname = "hl-alpine-compatibility";
          inherit version;
          src = workspaceSource;
          cargoLock.lockFile = ./Cargo.lock;
          strictDeps = true;
          nativeBuildInputs = commonNativeInputs pkgs ++ [
            pkgs.coreutils
            # `ps`. `product_checkpoint_test::process_tree` shells out to
            # `ps -axo pid=,ppid=` to learn the domain worker's process tree, and
            # `Command::new("ps")` on a host without it fails as a bare
            # `No such file or directory (os error 2)` with nothing naming `ps`.
            # Load-bearing, proven by removal rather than assumed: without it this gate
            # reddens at `total_failed_ms=62 error=No such file or directory (os error 2)`.
            pkgs.procps
            arm64Compiler
            (rustFor pkgs)
          ];
          doCheck = false;
          buildPhase = ''
            runHook preBuild
            export CARGO_BUILD_JOBS="$NIX_BUILD_CORES"
            export HL_PRODUCT_CHECKPOINT_REQUIRED=1
            # The daemon's public fixtures below currently target ARM64.  Use the
            # pinned guest compiler wrapper, which also supplies glibc's static
            # archive; a Nix sandbox deliberately has no /usr/bin compiler.
            export HL_GUEST_CC=${arm64Compiler}/bin/aarch64-linux-gnu-gcc

            run_ignored() {
              package="$1"
              test_target="$2"
              test_name="$3"
              shift 3
              timeout --kill-after=30s 10m \
                cargo test -p "$package" --test "$test_target" "$@" --locked --offline \
                "$test_name" -- --ignored --exact --nocapture --test-threads=1
            }

            run_alpine_gate() {
              export HL_SCENARIO_TARGET="$1"
              export HL_ALPINE_ARCHIVE="$2"

              # 19693a4c9 renamed `continue_later_restores_sleep_tree_in_two_fresh_domain_
              # processes` and did not update this line. `cargo test <filter> -- --exact`
              # exits 0 when the filter matches nothing, so from that commit until this one
              # both arms of this gate reported `running 0 tests ... test result: ok` and
              # the product's Continue-later contract was covered by nothing at all.
              # THE `exit status: 101` THIS COMMENT USED TO BLAME ON THE ENGINE WAS THE CA
              # PANIC BELOW. Every arm and both ISAs died identically ~70 ms in, before any
              # checkpoint work, so one shared cause was read as evidence of an engine defect
              # in the guest. Re-derived 2026-08-21 at 5d34dde49 with `--keep-failed`: the
              # worker's own log says `Client::new(): ... No CA certificates were loaded from
              # the system`. FIXED UPSTREAM in 6429c75f1, which made the registry client
              # lazy and fallible, so this derivation no longer exports a CA bundle to work
              # around a client it never uses.
              #
              # THE ISA CLAIM IS WITHDRAWN. It said the sleep-tree arm failed on arm64 while
              # passing on amd64. Measured outside the sandbox in the pinned dev shell, BOTH
              # ISAs pass -- amd64 and arm64 each 5 cycles, `1 passed`, ~12.2 s. Nothing here
              # is ISA-specific.
              #
              # WHAT IS STILL RED IS SANDBOX-ONLY, and it is now visible rather than masked.
              # Inside this derivation the worker publishes (start_ms=60) and the guest runs:
              # it writes its guard and all four identities, then loses all three background
              # `sleep` children before the first `kill -0`. The fixture's own marker reads
              # `SLEEP_CHILD_LOST` and the container records `exited code 91` ~100 ms in.
              # The fixture then waits out `budget(PHASE)` for a progress file that will never
              # be written, so on a loaded host `timeout` reaps it first and this step reports
              # exit 124 with NO `test result:` line at all -- read the preserved `$TMPDIR`,
              # not the exit code. That is an engine/sandbox interaction an engine lane owns,
              # and this gate exists to say so. Do not silence it by narrowing the filter back
              # to a name that matches nothing.
              for test_name in \
                continue_later_restores_the_primary_sleep_tree_across_repeated_cycles \
                continue_later_keeps_a_terminal_backed_pane_execution_across_repeated_cycles
              do
                timeout --kill-after=30s 10m \
                  cargo test -p husklet --features runtime --lib --locked --offline \
                  "runtime::domain::product_checkpoint_test::$test_name" \
                  -- --exact --nocapture --test-threads=1
              done
              run_ignored hl-container run_options process_run_options
              for test_name in \
                launch_contracts \
                sigterm_stop \
                exec_contracts \
                signalling_an_exec_does_not_stop_its_container \
                failed_exec_launches_are_process_local_and_retryable
              do
                run_ignored hl-container process_contract "$test_name"
              done
              for test_name in \
                new_file_is_visible \
                overwritten_file_is_visible \
                directory_tree_is_visible \
                held_directory_is_coherent
              do
                run_ignored hl-container filesystem_coherence "$test_name"
              done
              for test_name in \
                hangup_reaches_the_guest_signal_handler \
                configured_quit_reaches_the_guest_signal_handler \
                pause_stops_guest_progress_until_unpause \
                checkpoint_restore_preserves_filesystem_and_container_control \
                checkpoint_restore_restarts_interrupted_sleep_syscalls \
                health_probes_reach_healthy_and_unhealthy_states \
                a_descriptor_duplicated_with_fcntl_is_visible_in_proc_self_fd
              do
                run_ignored hl-container lifecycle_contract "$test_name"
              done

              # THIS ENUMERATION IS BY NAME, SO IT DRIFTS SILENTLY. A test added to
              # `lifecycle_contract.rs` is `#[ignore]`d -- it requires HL_ALPINE_ARCHIVE
              # -- so it runs in no ordinary `cargo test`, and it runs here only if
              # somebody remembers to type its name above. It exists, it passes, and it
              # is invisible to every runner: the same shape as a gate nobody invokes.
              # `a_descriptor_duplicated_with_fcntl_is_visible_in_proc_self_fd` reached
              # `main` in exactly that state and was added on 2026-08-20.
              #
              # So the count is asserted rather than trusted. Every test in that file is
              # `#[ignore]`d and no helper is, which makes the marker an exact census.
              # Add a test and this fails, naming the fix, instead of quietly covering
              # one fewer thing than it claims.
              lifecycle_source=src/containers/hl-container/tests/lifecycle_contract.rs
              declared="$(grep -c '#\[ignore' "$lifecycle_source")"
              if [ "$declared" -ne 7 ]; then
                printf '%s\n' \
                  "lifecycle_contract.rs declares $declared ignored tests; run_alpine_gate enumerates 7 by name." \
                  "Add the new test to the list above, then update this count. Until then it runs on no host." >&2
                exit 1
              fi
            }

            run_alpine_gate arm64 ${archives.arm64}
            run_alpine_gate amd64 ${archives.amd64}

            # The public daemon fixtures currently own an ARM64 container specification.
            # Keep their fixture-required tests explicit until that API accepts a guest target.
            export HL_SCENARIO_TARGET=arm64
            export HL_ALPINE_ARCHIVE=${archives.arm64}
            for test_name in \
              bridge_routing_contract \
              alpine_runtime_contracts \
              shell_vfork_exec_releases_parent_and_preserves_output \
              shared_mount_lock_contention \
              attach_runtime_contracts \
              failed_exec_upgrade_cleanup \
              descendant_cleanup
            do
              run_ignored hl-daemon daemon-api "$test_name" --features runtime
            done
            runHook postBuild
          '';
          installPhase = ''
            mkdir -p "$out"
            touch "$out/passed"
          '';
        };

      postgresCheckpointGateFor =
        pkgs:
        pkgs.writeShellApplication {
          name = "husklet-postgres-checkpoint-gate";
          runtimeInputs = [
            pkgs.coreutils
            pkgs.nix
          ];
          text = ''
            repository="$PWD"
            test -f "$repository/Cargo.lock" || {
              printf 'run this gate from the Husklet repository root\n' >&2
              exit 2
            }

            for target in arm64 amd64; do
              case "$target" in
                arm64) upper_target=ARM64 ;;
                amd64) upper_target=AMD64 ;;
              esac
              archive_name="HL_POSTGRES_''${upper_target}_ROOTFS_ARCHIVE"
              manifest_name="HL_POSTGRES_''${upper_target}_FIXTURE_MANIFEST"
              archive="''${!archive_name:-}"
              manifest="''${!manifest_name:-}"
              test -f "$archive" || {
                printf '%s must name the pinned %s PostgreSQL rootfs archive\n' "$archive_name" "$target" >&2
                exit 2
              }
              test -f "$manifest" || {
                printf '%s must name the pinned %s PostgreSQL fixture manifest\n' "$manifest_name" "$target" >&2
                exit 2
              }
              HL_SCENARIO_TARGET="$target" \
              HL_POSTGRES_ROOTFS_ARCHIVE="$archive" \
              HL_POSTGRES_FIXTURE_MANIFEST="$manifest" \
                timeout --kill-after=30s 15m \
                nix develop "$repository" --command \
                cargo test -p hl-container --test postgres_checkpoint --locked --offline \
                acceptance::postgres_survives_three_product_checkpoint_cycles -- \
                --ignored --exact --nocapture --test-threads=1
            done
          '';
        };

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
            cp ${workspaceSource}/src/runtime/hl-native/src/native/bridge/exports.txt "$TMPDIR/expected-exports"
            diff -u "$TMPDIR/expected-exports" "$TMPDIR/actual-exports"

            for name in hl-engine hl-aarch64 hl-x86_64
            do
              binary="$prefix/bin/$name"
              test -x "$binary"
              ! patchelf --print-needed "$binary" | grep -Fx libhl_native_engine.so >/dev/null
              patchelf --print-rpath "$binary" | tr : '\n' > "$TMPDIR/$name.runpath"
              while IFS= read -r entry; do
                if test -z "$entry"; then continue; fi
                case "$entry" in
                  /nix/store/*/lib) ;;
                  *) printf 'unsafe RUNPATH entry in %s: %s\n' "$name" "$entry" >&2; exit 1 ;;
                esac
              done < "$TMPDIR/$name.runpath"
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

            env -i PATH=/usr/bin:/bin HOME="$prefix/home" \
              "$prefix/bin/hl-engine" --backend-receipt > "$TMPDIR/receipt.json"

            chmod u+w "$prefix/lib"
            mv "$library" "$TMPDIR/libhl_native_engine.so"
            if env -i PATH=/usr/bin:/bin HOME="$prefix/home" \
              "$prefix/bin/hl-engine" --backend-receipt \
              > "$TMPDIR/missing-library.stdout" 2> "$TMPDIR/missing-library.stderr"; then
              printf '%s\n' 'engine started without its packaged sibling native library' >&2
              exit 1
            fi
            test ! -s "$TMPDIR/missing-library.stdout"
            mv "$TMPDIR/libhl_native_engine.so" "$library"

            mkdir -p "$out"
            cp "$TMPDIR/receipt.json" "$out/backend-receipt.json"
            (cd "$prefix" && sha256sum bin/hl-engine bin/hl-aarch64 bin/hl-x86_64 \
              lib/libhl_native_engine.so) > "$out/SHA256SUMS"
            printf '%s\n' \
              'copied-prefix explicit loader, no native NEEDED, backend ABI, and sibling-library selection passed' \
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
              src/runtime/hl-native/tests/host-abi/unix.c -L"$native_directory" \
              -Wl,-rpath-link,"$native_directory" -lhl_native_engine -o public-abi-c
            ${lib.escapeShellArg cxx} -std=c++20 -Wall -Wextra -Werror \
              -Isrc/runtime/hl-native/src/native/include \
              src/runtime/hl-native/tests/host-abi/unix.cpp -L"$native_directory" \
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
              -DHL_TARGET_NAMESPACE=${architecture} \
              -fsyntax-only src/runtime/hl-native/src/native/engine/target/${architecture}.c
            # WHAT THIS CHECK COVERS ON aarch64, AND WHY IT IS NARROWER THAN IT LOOKS.
            #
            # The `aarch64` attribute selects the aarch64 cross `cc` above, so the
            # -fsyntax-only pass really does compile the JIT arm. scan-build does NOT:
            # it substitutes its own NATIVE clang with the same argv, no `-target`
            # survives, so `__aarch64__` is undefined, `host/cpu.h` sets
            # HL_HOST_CPU_X86_64 and `engine/target/aarch64.c` takes its INTERPRETER
            # arm. The ~120 `e_*` emitters, the stubs, `translate.c` and `cache.c` are
            # analysed by neither attribute. That is why every dead-store finding this
            # check has ever produced was in `interp/`.
            #
            # SO: THE aarch64 ATTRIBUTE ANALYSES THE INTERPRETER ARM ONLY. A known-narrow
            # gate that says so is fine; one that looks broad and is not, is not.
            #
            # `--analyzer-target=aarch64-unknown-linux-gnu` DOES NOT FIX THIS -- measured
            # 2026-08-20, twice, and both shapes are worse than the status quo:
            #   - Flag alone: the analyzer keeps the NATIVE glibc headers, dies on
            #     `gnu/stubs.h: 'gnu/stubs-32.h' file not found`, and reports
            #     `1 error generated. / 0 bugs found.` WITH EXIT 0. Analyses nothing,
            #     says green. The assertion below exists because of this run.
            #   - Flag plus the cross libc on `-isystem`: it gets further and then fails
            #     with `use of undeclared identifier 'REG_RIP'` at
            #     `host/native_context.h:85`, which is inside
            #     `#elif defined(__linux__) && defined(HL_HOST_CPU_X86_64)`. The analyzer
            #     preprocessed as x86_64 while reading aarch64 libc headers -- a
            #     translation unit that is neither arm.
            # The flag sets the ANALYSIS TRIPLE, not the preprocessor macros that select
            # the code, and those macros are the entire mechanism. A real fix has to make
            # the analyzer genuinely target aarch64, headers and macros together.
            #
            # The JIT findings are DELIBERATELY UNSIZED. Nothing has ever analysed that
            # code, and an estimate would be a number nobody measured.
            set +e
            timeout 10m scan-build --status-bugs -o reports \
              ${lib.escapeShellArg compiler} \
              -O2 -fPIC -g -fno-omit-frame-pointer -std=c11 \
              -Isrc/runtime/hl-native/src/native \
              -Isrc/runtime/hl-native/src/native/include \
              -fvisibility=hidden \
              ${lib.escapeShellArgs portableWarnings} \
              -DHL_SHARED -DHL_BUILDING_ENGINE -DHL_ENABLE_LOGGING=0 \
              -DHL_TRANSLIT_DEFAULT=0 -D_GNU_SOURCE -DHL_EMBEDDED_BUILD=1 \
              -DHL_TARGET_NAMESPACE=${architecture} \
              -c src/runtime/hl-native/src/native/engine/target/${architecture}.c \
              -o engine.o 2>&1 | tee scan-build.log
            scan_status=''${PIPESTATUS[0]}
            set -e
            test "$scan_status" -eq 0

            # `--status-bugs` ANSWERS ONE QUESTION AND IT IS NOT THE ONE THIS GATE NEEDS.
            # It exits non-zero when the analyzer FINDS BUGS, and zero otherwise -- which
            # includes the case where the analyzer parsed nothing and had no opportunity
            # to find any. Measured 2026-08-20 while trying `--analyzer-target` below: the
            # run emitted `1 error generated.`, then `0 bugs found.`, and EXITED 0. The
            # derivation would have gone green over an analysis that never happened.
            #
            # So assert the two things an exit code cannot say: that the analyzer ran to
            # completion, and that it compiled the translation unit rather than dying in
            # it. Both hold on today's baseline -- this arch emits no analyzer errors --
            # so this is armed, not aspirational.
            grep -q 'Analysis run complete' scan-build.log || {
              printf '%s\n' 'scan-build did not report a completed analysis run' >&2
              exit 1
            }
            if grep -qE '[0-9]+ (error|fatal error)s? generated\.' scan-build.log; then
              printf '%s\n' 'the analyzer emitted errors, so a clean bug count proves nothing' >&2
              grep -E 'error:|[0-9]+ errors? generated\.' scan-build.log >&2
              exit 1
            fi
            mkdir -p "$out"
            printf '%s\n' \
              ${lib.escapeShellArg (
                "scan-build --status-bugs and strict C declaration/type/range/return diagnostics passed for the ${architecture} Linux unity translation unit"
                + lib.optionalString (architecture == "aarch64")
                  "; the analyzer covered the INTERPRETER arm only -- scan-build's substituted native clang leaves __aarch64__ undefined, so the JIT emitters were compiled by the -fsyntax-only pass but not analysed"
              )} \
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
            pkgs.jq
          ];
          buildInputs = [ windows.windows.mcfgthreads ];
          doCheck = false;
          buildPhase = ''
            runHook preBuild
            export CC_x86_64_pc_windows_gnu=${lib.escapeShellArg compiler}
            export CARGO_TARGET_${targetKey}_LINKER=${lib.escapeShellArg compiler}
            export CARGO_TARGET_${targetKey}_RUSTFLAGS=${lib.escapeShellArg "-Lnative=${windows.windows.pthreads}/lib"}
            export HL_NATIVE_COMPILE_CHECK=1
            cargo build --locked --offline --target ${target} -p hl-native -p hl-engine -p engine 2>&1 |
              tee "$TMPDIR/windows-contract.log"
            for executable in hl-engine hl-aarch64 hl-x86_64; do
              binary="target/${target}/debug/$executable.exe"
              test -s "$binary"
              ${windows.stdenv.cc.targetPrefix}objdump -f "$binary" \
                | grep -F 'file format pei-x86-64' >/dev/null
              ! ${windows.stdenv.cc.targetPrefix}objdump -p "$binary" \
                | grep -F 'DLL Name: hl_native_engine.dll' >/dev/null
              ${windows.stdenv.cc.targetPrefix}objdump -p "$binary" \
                | grep -F 'LoadLibraryExW' >/dev/null
              ${windows.stdenv.cc.targetPrefix}objdump -p "$binary" \
                | grep -F 'GetProcAddress' >/dev/null
            done
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
            ${lib.escapeShellArg compiler} -std=c11 -Wall -Wextra -Werror \
              -DHL_SHARED \
              -Isrc/runtime/hl-native/src/native \
              -Isrc/runtime/hl-native/src/native/include \
              -Isrc/runtime/hl-native/src/native/bridge \
              src/runtime/hl-native/tests/windows_bridge_abi.c "$import" \
              -o checkpoint-bridge-contract.exe
            ${windows.stdenv.cc.targetPrefix}objdump -f checkpoint-bridge-contract.exe \
              | grep -F 'file format pei-x86-64' >/dev/null
            ${windows.stdenv.cc.targetPrefix}objdump -p checkpoint-bridge-contract.exe \
              | grep -F 'DLL Name: hl_native_engine.dll' >/dev/null
            for symbol in \
              hl_c_backend_checkpoint_adopt \
              hl_c_backend_checkpoint_broker_accept \
              hl_c_backend_checkpoint_broker_pair \
              hl_c_backend_checkpoint_configure \
              hl_c_backend_checkpoint_interrupt_signal \
              hl_c_backend_checkpoint_trigger_bump \
              hl_c_backend_checkpoint_trigger_create \
              hl_c_backend_checkpoint_trigger_destroy; do
              ${windows.stdenv.cc.targetPrefix}objdump -p checkpoint-bridge-contract.exe \
                | grep -F "$symbol" >/dev/null
            done
            # HALF THIS CRATE'S WINDOWS TEST SURFACE IS EMPTY, and no artifact count can
            # see it: of `hl-native`'s 46 test targets, 23 compile to empty crates on a
            # Windows check without `native-test-hooks` -- 18 feature-gated and 5
            # platform-gated -- while ALL 46 still emit artifacts. `identity_registry` is
            # the only one deliberately armed FOR Windows, which is why it is named here
            # rather than left to `--all-targets`. Unifying the feature through
            # `-p hl-engine --all-targets` makes 18 of the 23 live; the other 5 are honest
            # platform exclusions.
            cargo test --locked --offline --target ${target} -p hl-native \
              --test identity_registry --no-run
            ${lib.escapeShellArg compiler} -std=c11 -DHL_SHARED -DHL_BUILDING_ENGINE \
              -Isrc/runtime/hl-native/src/native -Isrc/runtime/hl-native/src/native/include \
              -c src/runtime/hl-native/src/native/bridge/host.c -o host-bridge.obj
            ${windows.stdenv.cc.targetPrefix}objdump -f host-bridge.obj \
              | grep -F 'file format pe-x86-64' >/dev/null
            ${windows.stdenv.cc.targetPrefix}objdump -f host-bridge.obj \
              | grep -F 'architecture: i386:x86-64' >/dev/null
            mkdir windows-host-objects
            for source in src/runtime/hl-native/src/native/host/windows/*.c; do
              # io.c is a private fragment included by file.c, not an independent
              # translation unit. Cargo's source inventory excludes included C
              # files for the same reason.
              [ "$(basename "$source")" = io.c ] && continue
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
              -c src/runtime/hl-native/tests/host-abi/windows.c -o public-abi-c.obj
            ${windows.stdenv.cc.targetPrefix}objdump -f public-abi-c.obj \
              | grep -F 'file format pe-x86-64' >/dev/null
            ${windows.stdenv.cc.targetPrefix}objdump -f public-abi-c.obj \
              | grep -F 'architecture: i386:x86-64' >/dev/null
            ${lib.escapeShellArg cxx} -std=c++20 -Wall -Wextra -Werror \
              -DHL_SHARED -Isrc/runtime/hl-native/src/native/include \
              -c src/runtime/hl-native/tests/host-abi/windows.cpp -o public-abi-cxx.obj
            ${windows.stdenv.cc.targetPrefix}objdump -f public-abi-cxx.obj \
              | grep -F 'file format pe-x86-64' >/dev/null
            ${windows.stdenv.cc.targetPrefix}objdump -f public-abi-cxx.obj \
              | grep -F 'architecture: i386:x86-64' >/dev/null
            ${lib.escapeShellArg compiler} -std=c11 -DHL_SHARED -DHL_BUILDING_ENGINE \
              -DHL_ABI_FIXTURE_EXPORT \
              -Isrc/runtime/hl-native/src/native/include \
              -L${windows.windows.mcfgthreads}/lib \
              -shared src/runtime/hl-native/tests/host-abi/windows.c -o hl-abi-fixture.dll \
              -Wl,--out-implib,libhl-abi-fixture.dll.a
            ${windows.stdenv.cc.targetPrefix}objdump -f hl-abi-fixture.dll \
              | grep -F 'file format pei-x86-64' >/dev/null
            ${windows.stdenv.cc.targetPrefix}objdump -f hl-abi-fixture.dll \
              | grep -F 'architecture: i386:x86-64' >/dev/null
            file hl-abi-fixture.dll | grep -E 'PE32\+.*DLL.*x86-64'
            file libhl-abi-fixture.dll.a | grep -F 'current ar archive'
            ${windows.stdenv.cc.targetPrefix}nm -g libhl-abi-fixture.dll.a \
              | grep -F ' T hl_ci_engine_abi' >/dev/null

            # ---------------------------------------------------------------------------
            # THE cfg-WIDTH GATE, READ AS A COUNT AND NEVER AS AN EXIT CODE.
            #
            # `cargo check --all-targets` exits 0 while SILENTLY SKIPPING a target whose
            # required features are unmet. Measured rather than argued: clamping one target
            # with a `required-features` it does not have left cargo at exit 0 while the
            # compiled-unit count fell 87 to 86. An exit status cannot see that; a census
            # can. So this arm diffs a per-crate unit count against a pinned table, and the
            # `diff` IS the assertion -- nothing here reads `$?` from cargo.
            #
            # Shown to fail before landing, on the captured JSON of a real run: removing a
            # single `hl-native` artifact takes the table from `48 hl-native` to
            # `47 hl-native` and `diff -u` exits 1 naming the crate.
            #
            # Sorted by crate name so the comparison cannot fail on ordering alone. The
            # first pair of files produced for this gate differed ONLY in the order of
            # `engine` and `extension`, which would have been a spurious red.
            #
            # WHEN THIS REDDENS ON A TARGET YOU ADDED, that is the gate working: read the
            # `diff`, confirm the new target belongs in the Windows census, and update the
            # number below. Do NOT widen the pipeline to make the count float, which would
            # return this arm to reading nothing. It reddened FOR REAL within minutes of
            # being written: `imported_path_guard.rs` arrived on `main` from the chmod
            # EFAULT lane and took `hl-native` from 48 to 49, unplanted.
            #
            # WHAT THIS WHOLE ATTRIBUTE IS AND IS NOT. Every claim here is COMPILE AND LINK
            # evidence. Three DLLs and a PE32+ executable are produced and inspected, and
            # NOT ONE INSTRUCTION HAS RUN -- there is no Windows host in this build. Whether
            # the exported symbols behave, and whether `CreateProcess`-based spawning works,
            # are open questions that need a real Windows runner. Do not let a green here be
            # read as a working Windows product.
            cargo check --locked --offline --target ${target} --all-targets \
              --message-format=json \
              -p hl-native -p hl-engine -p engine -p hl-fs -p hl-log -p hl-process \
              -p hl-rpc -p hl-design -p hl-cc -p hl-gui -p hl-ws -p hl-ws-term \
              -p hl-extension -p extension > windows-units.json
            jq -r '
              select(.reason == "compiler-artifact")
              | select(.package_id | startswith("path+"))
              | ((.package_id | capture("#(?<n>[A-Za-z0-9_-]+)@") | .n)
                 // (.package_id | split("#")[0] | split("/") | last))
                + "\t" + .target.name + "\t" + (.target.kind | join(","))
            ' windows-units.json | sort -u | cut -f1 | sort | uniq -c \
              | sed 's/^ *//' > windows-units.actual
            cat > windows-units.expected <<'WINDOWS_UNITS'
6 engine
3 extension
1 hl-cc
2 hl-design
5 hl-engine
7 hl-extension
1 hl-fs
8 hl-gui
2 hl-log
50 hl-native
1 hl-process
1 hl-rpc
1 hl-ws
2 hl-ws-term
WINDOWS_UNITS
            diff -u windows-units.expected windows-units.actual
            runHook postBuild
          '';
          installPhase = ''
            mkdir -p "$out"
            printf '%s\n' \
              'GNU Windows hl-native/hl-engine Rust target compile, engine executables use the explicit secure loader without a direct engine-DLL import, the C bridge links through the generated import library, complete engine DLL/import-library link with exact public exports, every Windows host-service translation unit, forced POSIX compatibility, and strict C/C++ public-header contracts; this is compile/link evidence, not MSVC SDK or runtime proof' \
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
            ! otool -L "$binary" | grep -F 'libhl_native_engine.dylib' >/dev/null
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
          mv "$TMPDIR/libhl_native_engine.dylib" "$library"
          mkdir -p "$out"
          printf '%s\n' 'native Darwin copied-prefix exact ARM64 architecture, install name, exports, explicit loader, deterministic hash-bound backend receipts, and sibling-library isolation passed' > "$out/evidence"
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
              ${workspaceSource}/src/runtime/hl-native/tests/host-abi/unix.c \
              -L"$TMPDIR/product" -lhl_native_engine \
              -Wl,-rpath,@loader_path -o "$TMPDIR/product/public-abi-c"
            ${pkgs.stdenv.cc}/bin/c++ -std=c++20 -Wall -Wextra -Werror \
              -DHL_SHARED -I${workspaceSource}/src/runtime/hl-native/src/native/include \
              ${workspaceSource}/src/runtime/hl-native/tests/host-abi/unix.cpp \
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
          "workspace-source" = workspaceSourceContractFor pkgs;
          package = verification;
          workspace = verification;
          test = verification;
          "design-lint" = verification;
          "lint-cases" = verification;
          "compat-fixtures" = verification;
          "native-test-hooks" = nativeHookVerificationFor pkgs;
        }
        // lib.optionalAttrs pkgs.stdenv.isLinux {
          "alpine-compatibility" = alpineCompatibilityFor pkgs;
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
        postgres-checkpoint-gate = {
          type = "app";
          program = "${postgresCheckpointGateFor pkgs}/bin/husklet-postgres-checkpoint-gate";
        };
      });

      devShells = forAllSystems (
        pkgs:
        let
          toolchain = toolchainFor pkgs;
          alpineArchives = alpineArchivesFor pkgs;
          alpine = if pkgs.stdenv.isLinux then linuxAlpineFor pkgs else null;
          # The same `pkgsCross.mingwW64` the `host-windows-x86_64-gnu-smoke` check already
          # builds against, so this adds no closure CI does not already substitute.
          windows = pkgs.pkgsCross.mingwW64;
          windowsCc = "${windows.stdenv.cc}/bin/${windows.stdenv.cc.targetPrefix}cc";
          windowsCxx = "${windows.stdenv.cc}/bin/${windows.stdenv.cc.targetPrefix}c++";
          windowsAr = "${windows.stdenv.cc}/bin/${windows.stdenv.cc.targetPrefix}ar";
          windowsLibraries = "-L${windows.windows.mcfgthreads}/lib -L${windows.windows.pthreads}/lib";
        in
        {
          default = pkgs.mkShell (
            toolchain.env
            // {
              packages = [
                pkgs.clang-tools
                pkgs.cppcheck
                pkgs.git
                pkgs.go
                pkgs.nixfmt
                # The extensions under extensions/ are JavaScript packages with
                # their own tests; without a runtime they can only be checked on
                # a machine that happens to have one.
                pkgs.nodejs_22
                pkgs.pkg-config
                (rustFor pkgs)
              ]
              ++ lib.optionals toolchain.canBuildGuests (
                toolchain.crossCompilers
                ++ toolchain.rustStaticLinkers
                # The `<isa>-linux-gnu-gcc` aliases were Linux-only, so every test that builds a guest
                # by that spelling -- `hl-native`'s `engine::tests` re-exec pair among them -- hard
                # failed on the Darwin shell even though the cross compilers themselves were already
                # in it. macOS is the host the product ships on; those tests must run there.
                ++ toolchain.compilerAliases
                ++ lib.optionals pkgs.stdenv.isLinux toolchain.guestLibraries
                ++ toolchain.emulators
              )
              # A Linux development host has no monitor, and GTK exits when it cannot open a display,
              # so the application could be type-checked here but never started. Xvfb supplies one:
              # `xvfb-run -a -s '-screen 0 1600x1000x24' -- husklet` runs the real GSK pipeline, and
              # `HL_TERM_SHOT` then writes the rendered window to a PNG. See AGENTS.md.
              ++ lib.optionals pkgs.stdenv.isLinux [
                pkgs.xorg-server
                pkgs.xvfb-run
                # WHY THE WINDOWS C SURFACE WAS FOLKLORE UNTIL NOW.
                #
                # `cargo check --target x86_64-pc-windows-gnu` compiles ZERO C on its own:
                # `HostTarget::supported()` is false for Windows, so `native_build.rs` returns at
                # `emit_planned_target` and stubs the fingerprint to `unbuilt`. Every "I checked the
                # Windows surface" that used that command covered Rust only. Setting
                # `HL_NATIVE_COMPILE_CHECK=1` asks for the C, and without a mingw compiler on PATH it
                # then dies in `bridge/shim.c` against host glibc headers -- which reads like the
                # target being impossible rather than the toolchain being absent. Two lanes have now
                # concluded "unverifiable on this box"; it is verifiable, and this is what it needed.
                windows.stdenv.cc
              ];
              shellHook = ''
                export CC="${toolchain.env.CC}"
                export CXX="${toolchain.env.CXX}"
                export NATIVE_CC="${toolchain.env.NATIVE_CC}"
                export NATIVE_CXX="${toolchain.env.NATIVE_CXX}"
                native_c_target="$($CC -dumpmachine)"
                native_cxx_target="$($CXX -dumpmachine)"
                if [ "$native_c_target" != "$native_cxx_target" ]; then
                  printf 'native compiler target mismatch: CC=%s CXX=%s\n' \
                    "$native_c_target" "$native_cxx_target" >&2
                  return 1
                fi
                export CARGO_BUILD_JOBS="''${CARGO_BUILD_JOBS:-1}"
                export HL_COMPAT_JOBS="''${HL_COMPAT_JOBS:-1}"
              '' + lib.optionalString pkgs.stdenv.isLinux ''
                export CC_x86_64_pc_windows_gnu=${lib.escapeShellArg windowsCc}
                export CXX_x86_64_pc_windows_gnu=${lib.escapeShellArg windowsCxx}
                export AR_x86_64_pc_windows_gnu=${lib.escapeShellArg windowsAr}
                export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=${lib.escapeShellArg windowsCc}
                # `rustc` links the .exe; the two -L below are what it needs.
                export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS=${
                  lib.escapeShellArg "-Lnative=${windows.windows.pthreads}/lib -Lnative=${windows.windows.mcfgthreads}/lib"
                }
                # AND THE PIECE THAT IS MISSING EVERYWHERE. `hl-native`'s build script links
                # `hl_native_engine.dll` ITSELF, through the compiler above rather than through
                # `rustc`, so RUSTFLAGS never reaches it and the build dies on
                # `cannot find -lmcfgthread` -- an error that names no crate and no target and has
                # cost more time than anything else on this surface. In the
                # `host-windows-x86_64-gnu-smoke` derivation this comes free from
                # `buildInputs = [ windows.windows.mcfgthreads ]`; a devShell has to say it. The
                # variable is the cross wrapper's own suffix-salted spelling, taken from
                # `cc.suffixSalt` rather than hand-spelled so it cannot drift from the triple.
                export NIX_LDFLAGS_${windows.stdenv.cc.suffixSalt}=${lib.escapeShellArg windowsLibraries}
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
