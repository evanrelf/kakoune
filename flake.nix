{
  inputs = {
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
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
                ./share
                ./test
                (lib.fileset.fileFilter (file: file.hasExt "cc" || file.hasExt "hh") ./src)
              ];
            };

            makeFlags = [
              "PREFIX=${placeholder "out"}"
              "version=${config.packages.default.version}"
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

        checks.default = config.packages.default;

        devShells.default =
          pkgs.mkShell {
            inputsFrom = [ config.packages.default ];
            packages = with pkgs; [
              clang-tools
            ];
          };
      };
    };
}
