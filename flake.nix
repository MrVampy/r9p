{
  description = "Reusable Rust 9P2000 protocol primitives";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem
      (system:
        let
          pkgs = import nixpkgs {
            inherit system;
          };
          r9p = pkgs.rustPlatform.buildRustPackage {
            pname = "r9p";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
          };
        in
        {
          packages.default = r9p;
          packages.r9p = r9p;

          devShells.default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              rust-analyzer
              git
              jq
              ripgrep
              nixpkgs-fmt
            ];

            shellHook = ''
              echo "r9p dev shell"
              echo "Tools: cargo, rustc, rustfmt, clippy, rust-analyzer, git, jq, rg"
            '';
          };

          formatter = pkgs.nixpkgs-fmt;
        });
}
