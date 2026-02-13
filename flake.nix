{
  description = "wt-core — portable Git worktree lifecycle manager";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          fs = pkgs.lib.fileset;
          src = fs.toSource {
            root = ./.;
            fileset = fs.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./tests
              ./bindings
            ];
          };
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "wt-core";
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

            inherit src;

            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = [ pkgs.git pkgs.makeWrapper ];

            postInstall = ''
              mkdir -p $out/share/wt-core
              cp -r bindings $out/share/wt-core/bindings
              wrapProgram $out/bin/wt-core \
                --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.git ]}
            '';

            meta = with pkgs.lib; {
              description = "Portable Git worktree lifecycle manager";
              homepage = "https://github.com/kioku/wt-core";
              license = licenses.mit;
              mainProgram = "wt-core";
            };
          };
        });

      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            buildInputs = [
              pkgs.cargo
              pkgs.rustc
              pkgs.clippy
              pkgs.rustfmt
              pkgs.git
            ];
          };
        });
    };
}
