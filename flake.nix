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
              clang
              rustc
              rustfmt
              clippy
              rust-analyzer
              just
              git
              jq
              ripgrep
              nixpkgs-fmt
              # Agent-loop tooling — same set used in Racme + r9pfuse.
              # See justfile for tier-by-tier usage.
              mold
              sccache
              cargo-nextest
              cargo-deny
              cargo-machete
              cargo-mutants
              cargo-outdated
              cargo-expand
            ];
          };

          formatter = pkgs.nixpkgs-fmt;
        });
}
