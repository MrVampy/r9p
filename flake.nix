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
            nativeBuildInputs = with pkgs; [
              clang
              mold
              binutils
            ];
          };
          front = pkgs.rustPlatform.buildRustPackage {
            pname = "r9p-front";
            version = "0.1.0-abi20";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "front" ];
            doCheck = false;
            nativeBuildInputs = with pkgs; [
              clang
              mold
              binutils
            ];
            installPhase = ''
              runHook preInstall
              install -Dm644 target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/libfront.so \
                "$out/lib/libfront.so"
              install -Dm644 crates/front/include/r9p_front.h \
                "$out/include/r9p_front.h"
              install -Dm644 crates/front/bindings/deno/front_sink.ts \
                "$out/share/r9p/front/deno/front_sink.ts"
              install -Dm644 crates/front/bindings/deno/export_descriptor.ts \
                "$out/share/r9p/front/deno/export_descriptor.ts"
              install -Dm644 crates/front/bindings/deno/request_context.ts \
                "$out/share/r9p/front/deno/request_context.ts"
              runHook postInstall
            '';
          };
          beamPort = pkgs.rustPlatform.buildRustPackage {
            pname = "r9p-beam-port";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "beam-port" ];
            nativeBuildInputs = with pkgs; [
              clang
              mold
              binutils
            ];
            installPhase = ''
              runHook preInstall
              install -Dm755 target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/r9p-beam-port \
                "$out/bin/r9p-beam-port"
              runHook postInstall
            '';
          };
          beamGleam = pkgs.stdenvNoCC.mkDerivation {
            pname = "r9p-beam-gleam";
            version = "0.1.0";
            src = ./bindings/gleam;
            dontBuild = true;
            installPhase = ''
              runHook preInstall
              mkdir -p "$out/share/r9p/beam/gleam"
              cp -R . "$out/share/r9p/beam/gleam/"
              runHook postInstall
            '';
          };
          frontTests = pkgs.rustPlatform.buildRustPackage {
            pname = "r9p-front-tests";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "front" "-p" "beam-port" ];
            cargoTestFlags = [ "-p" "front" "-p" "beam-port" ];
            nativeBuildInputs = with pkgs; [
              clang
              mold
              binutils
            ];
            installPhase = ''
              runHook preInstall
              mkdir -p "$out"
              printf 'passed\n' > "$out/result"
              runHook postInstall
            '';
          };
        in
        {
          packages.default = r9p;
          packages.beam = pkgs.symlinkJoin {
            name = "r9p-beam";
            paths = [ beamPort beamGleam ];
          };
          packages.beam-gleam = beamGleam;
          packages.beam-port = beamPort;
          packages.front = front;
          packages.front-tests = frontTests;
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
              # Agent-loop tooling — same set used across the sibling workspaces.
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
