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
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "wt-core";
            version = "0.1.0";

            src = ./.;

            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = [ pkgs.git ];

            propagatedBuildInputs = [ pkgs.git ];

            postInstall = ''
              mkdir -p $out/share/wt-core
              cp -r bindings $out/share/wt-core/bindings
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
