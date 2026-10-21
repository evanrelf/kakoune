{
  description = "kakrs: Kakoune ported to Rust";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          inherit (pkgs) lib;
          # %val{version}: the C++ uses `git describe`, unavailable in the sandbox
          rev = self.shortRev or self.dirtyShortRev or "unknown";
        in
        {
          default = self.packages.${system}.kakrs;

          # The Rust port, installed with the runtime files (kakrc, rc,
          # colors, doc) in share/kak, the layout of the C++ `make install`:
          # kak finds them at <bin>/../share/kak, so no KAKOUNE_RUNTIME is
          # needed.
          kakrs = pkgs.rustPlatform.buildRustPackage {
            pname = "kakrs";
            version = "0-unstable-${lib.substring 0 8 self.lastModifiedDate}";

            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./kakrs/Cargo.toml
                ./kakrs/Cargo.lock
                ./kakrs/build.rs
                ./kakrs/src
                ./share/kak/kakrc
                ./rc
                ./colors
                ./doc/pages
              ];
            };
            cargoRoot = "kakrs";
            buildAndTestSubdir = "kakrs";
            cargoLock.lockFile = ./kakrs/Cargo.lock;

            # build.rs reads the version from here, like the C++ Makefile
            preBuild = ''
              echo ${rev} > .version
            '';

            # The hegel property tests need a hegel server, which the
            # sandbox can't fetch; run `cargo test` in a dev shell instead.
            doCheck = false;

            postInstall = ''
              share=$out/share/kak
              mkdir -p $share/doc
              cp share/kak/kakrc $share/
              cp -r rc colors $share/
              cp doc/pages/*.asciidoc $share/doc/
              ln -s rc $share/autoload

              # Rename the binary so it can be installed next to the C++ kak.
              # Drop this (and point the libexec link and mainProgram back at
              # kak) to undo the rename.
              mv $out/bin/kak $out/bin/kakrs

              # Shell scripts run by kak (%sh{}, rc/tools, windowing) call
              # `kak -p`/`kak -c` by name: kak prepends <bin>/../libexec/kak to
              # their PATH, so this link makes them reach this binary rather
              # than another kak on the PATH (same as the C++ make install).
              mkdir -p $out/libexec/kak
              ln -s ../../bin/kakrs $out/libexec/kak/kak
            '';

            meta = {
              description = "Kakoune, the modal code editor, ported to Rust";
              homepage = "https://kakoune.org";
              license = lib.licenses.unlicense;
              mainProgram = "kakrs";
            };
          };
        });

      devShells = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.kakrs ];
            packages = [ pkgs.clippy pkgs.rustfmt ];
          };
        });
    };
}
