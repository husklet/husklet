{
  description = "Husklet with the integrated Rust execution engine";

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
          ./lint
          ./rust-toolchain.toml
          ./rustfmt.toml
          ./src
          ./tests
        ];
      };

      commonNativeInputs = pkgs: [
        pkgs.pkg-config
        pkgs.python3
      ];

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
            gnumake
            cmake
            pkg-config
            clang
            python3
            nodejs
            go
            rustc
            cargo
          ]);
        };

      alpineFor =
        pkgs:
        pkgs.fetchurl {
          url = "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/aarch64/alpine-minirootfs-3.24.1-aarch64.tar.gz";
          hash = "sha256-9VqQ9pBSxb1vkssJqPRwZZcIMLGUyRegBvuUAo5yElk=";
        };

      packageFor =
        pkgs:
        (rustPlatformFor pkgs).buildRustPackage {
          pname = "hl-engine";
          inherit version;
          src = workspaceSource;
          cargoLock.lockFile = ./Cargo.lock;
          strictDeps = true;
          nativeBuildInputs = commonNativeInputs pkgs;
          CARGO_BUILD_JOBS = "1";
          doCheck = false;
          buildPhase = ''
            runHook preBuild
            cargo build --release -p engine --bins --locked --offline -j 1
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            mkdir -p "$out/bin"
            for binary in hl-engine hl-aarch64 hl-x86_64
            do
              install -Dm755 "target/release/$binary" "$out/bin/$binary"
            done
            runHook postInstall
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
        (rustPlatformFor pkgs).buildRustPackage {
          pname = "hl-engine-verification";
          inherit version;
          src = workspaceSource;
          cargoLock.lockFile = ./Cargo.lock;
          strictDeps = true;
          nativeBuildInputs = commonNativeInputs pkgs ++ [
            (rustFor pkgs)
          ];
          CARGO_BUILD_JOBS = "1";
          HL_COMPAT_JOBS = "1";
          doCheck = false;
          buildPhase = ''
            runHook preBuild

            cargo fmt --all --check --message-format short
            cargo run --locked --offline -q -p hl-design-lint -- src
            cargo run --locked --offline -q -p hl-design-lint -- --cases lint src
            cargo build -p engine -p testing --bins --locked --offline -j 1
            export HL_TEST_ENGINE_APP_BIN_DIR="$PWD/target/debug"
            cargo check --workspace --all-targets --locked --offline -j 1
            cargo clippy --workspace --all-targets --locked --offline -j 1 -- -D warnings
            cargo test --workspace --all-targets --locked --offline -j 1 --no-fail-fast
            cargo test --workspace --doc --locked --offline -j 1

            python3 tests/runtime/legacy/corpus.py verify
            python3 tests/runtime/legacy/fixture_schema.py --check
            python3 tests/runtime/legacy/priority.py --check
            PYTHONPATH=tests/runtime/legacy python3 -m unittest \
              tests/runtime/legacy/corpus_test.py \
              tests/runtime/legacy/fixture_schema_test.py \
              tests/runtime/legacy/priority_test.py

            runHook postBuild
          '';
          installPhase = ''
            mkdir -p "$out"
            touch "$out/passed"
          '';
        };
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
        in
        {
          package = self.packages.${pkgs.stdenv.hostPlatform.system}.engine;
          workspace = verification;
          test = verification;
          "design-lint" = verification;
          "lint-cases" = verification;
          "compat-fixtures" = verification;
        }
      );

      devShells = forAllSystems (
        pkgs:
        let
          toolchain = toolchainFor pkgs;
        in
        {
          default = pkgs.mkShell (
            toolchain.env
            // {
              packages = [
                pkgs.gnumake
                pkgs.nixfmt
                pkgs.pkg-config
                pkgs.python3
                (rustFor pkgs)
              ]
              ++ lib.optionals toolchain.canBuildGuests (
                toolchain.crossCompilers ++ toolchain.compilerAliases ++ toolchain.emulators
              );
              shellHook = ''
                export CC="${toolchain.env.CC}"
                export NATIVE_CC="${toolchain.env.NATIVE_CC}"
                export CARGO_BUILD_JOBS="''${CARGO_BUILD_JOBS:-1}"
                export HL_COMPAT_JOBS="''${HL_COMPAT_JOBS:-1}"
              '';
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
              HL_ALPINE_ARCHIVE = alpineFor pkgs;
            }
          );
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
