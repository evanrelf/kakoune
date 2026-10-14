{
  inputs = {
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    # The upstream commit this fork branched from, as a reference to test the
    # Rust port against
    kakoune-upstream = {
      url = "github:mawww/kakoune/4d4ee86c2d039c2a5f29f109e7e59105d2073046";
      flake = false;
    };
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    systems.url = "github:nix-systems/default";
  };

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      systems = import inputs.systems;

      perSystem = { config, lib, pkgs, ... }: {
        packages.default =
          pkgs.stdenv.mkDerivation {
            pname = "kakoune";
            version = inputs.self.shortRev or inputs.self.dirtyShortRev or "unknown";

            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Makefile
                ./README.asciidoc
                ./colors
                ./doc
                ./rc
                ./rust/Cargo.lock
                ./rust/Cargo.toml
                ./rust/build.rs
                ./rust/hegel.toml
                ./rust/reference
                ./rust/src
                ./share
                ./test
                (lib.fileset.fileFilter (file: file.hasExt "cc" || file.hasExt "hh") ./src)
              ];
            };

            cargoRoot = "rust";

            cargoDeps = pkgs.rustPlatform.importCargoLock {
              lockFile = ./rust/Cargo.lock;
            };

            nativeBuildInputs = [
              pkgs.cargo
              pkgs.rustPlatform.cargoSetupHook
              pkgs.rustc
            ];

            makeFlags = [
              "PREFIX=${placeholder "out"}"
              "version=${config.packages.default.version}"
              "CARGOFLAGS=--frozen"
            ];

            enableParallelBuilding = true;

            doCheck = true;

            preCheck = ''
              # Word-character tests need a UTF-8 locale
              export LC_ALL=C.UTF-8
              # Some tests invoke `kak` from `PATH`
              export PATH="$PWD/src:$PATH"
            '';

            doInstallCheck = true;

            installCheckPhase = ''
              runHook preInstallCheck
              $out/bin/kak -ui json -e "kill 0"
              runHook postInstallCheck
            '';

            meta.mainProgram = "kak";
          };

        packages.kakoune-upstream =
          pkgs.stdenv.mkDerivation {
            pname = "kakoune-upstream";
            version = inputs.kakoune-upstream.shortRev;

            src = inputs.kakoune-upstream;

            makeFlags = [
              "PREFIX=${placeholder "out"}"
              "version=${config.packages.kakoune-upstream.version}"
            ];

            enableParallelBuilding = true;

            meta.mainProgram = "kak";
          };

        checks.default = config.packages.default;

        checks.rust =
          pkgs.stdenv.mkDerivation {
            name = "kakoune-rust-tests";

            inherit (config.packages.default) src cargoRoot cargoDeps nativeBuildInputs;

            KAKOUNE_UPSTREAM_SRC = inputs.kakoune-upstream;
            # Character classification tests need a UTF-8 locale
            LC_ALL = "C.UTF-8";
            # Far more cases than local runs, from a fixed seed so failures
            # reproduce with the same variables
            HEGEL_TEST_CASES = 100000;
            HEGEL_SEED = 0;

            buildPhase = ''
              runHook preBuild
              cargo test --manifest-path rust/Cargo.toml --workspace --frozen
              runHook postBuild
            '';

            installPhase = "touch $out";
          };

        devShells.default =
          pkgs.mkShell {
            inputsFrom = [ config.packages.default ];
            packages = with pkgs; [
              clang-tools
              clippy
              rust-analyzer
              rustfmt
            ];
            # For `rust/reference`
            KAKOUNE_UPSTREAM_SRC = inputs.kakoune-upstream;
          };
      };
    };
}
