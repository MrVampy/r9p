{
  description = "Reusable Rust 9P2000 protocol primitives";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    {
      lib = {
        skills.r9p = builtins.path {
          path = ./skills/r9p;
          name = "r9p-skill";
        };
        nativeSkills.r9p =
          let
            source = self.lib.skills.r9p;
          in
          {
            owner = "r9p";
            canonicalName = "r9p";
            inherit source;
            installedNames = {
              codex = "r9p";
              claude = "r9p";
            };
            requiredNamespacePaths = [ ];
          };
      };

      nixosModules.session-auth =
        { pkgs, ... }@moduleArgs:
        import ./nix/session-auth.nix (moduleArgs // {
          defaultPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        });
    }
    // flake-utils.lib.eachDefaultSystem
      (system:
        let
          pkgs = import nixpkgs {
            inherit system;
          };
          rustSource = nixpkgs.lib.fileset.toSource {
            root = ./.;
            fileset = nixpkgs.lib.fileset.unions [
              ./.cargo
              ./Cargo.lock
              ./Cargo.toml
              ./crates
            ];
          };
          sessionAuthModuleEval = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              self.nixosModules.session-auth
              {
                services.r9p-session-auth.keys.proof = {
                  privateKeyFile = "/var/lib/r9p-session-auth/proof.key";
                  publicKeyFile = "/var/lib/r9p-session-auth/proof.key.pub";
                  user = "r9p-proof";
                  group = "r9p-proof";
                };
                services.r9p-session-auth.keys.shared-proof = {
                  privateKeyFile = "/var/lib/r9p-session-auth-shared/proof.key";
                  publicKeyFile = "/var/lib/r9p-session-auth-shared/proof.key.pub";
                  user = "r9p-proof";
                  group = "r9p-shared";
                  directoryMode = "0750";
                  privateKeyAccess = "owner-group-read";
                };
                system.stateVersion = "25.11";
              }
            ];
          };
          r9p = pkgs.rustPlatform.buildRustPackage {
            pname = "r9p";
            version = "0.1.0";
            src = rustSource;
            requiredSystemFeatures = pkgs.lib.optionals (
              pkgs.stdenv.hostPlatform.system == "x86_64-linux"
            ) [ "cloud-burst" ];
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = with pkgs; [
              clang
              mold
              binutils
              makeWrapper
            ];
            nativeCheckInputs = with pkgs; [
              git
            ];
            postFixup = ''
              wrapProgram "$out/bin/r9p" \
                --suffix PATH : ${pkgs.lib.makeBinPath [ pkgs.fuse3 ]}
            '';
          };
          front = pkgs.rustPlatform.buildRustPackage {
            pname = "r9p-front";
            version = "0.1.0-abi23";
            src = rustSource;
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
            src = rustSource;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "beam-port" ];
            doCheck = false;
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
            src = rustSource;
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

          checks = pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            r9p = r9p;
            beam-front = frontTests;
            fuse-runtime-helper = pkgs.runCommandLocal "r9p-fuse-runtime-helper-check" { } ''
              grep -F ${pkgs.lib.escapeShellArg "${pkgs.fuse3}/bin"} ${r9p}/bin/r9p
              grep -F 'PATH=$PATH' ${r9p}/bin/r9p
              touch "$out"
            '';
            session-auth-module =
              let
                service = sessionAuthModuleEval.config.systemd.services.r9p-session-key-proof;
                sharedService = sessionAuthModuleEval.config.systemd.services.r9p-session-key-shared-proof;
              in
              pkgs.runCommandLocal "r9p-session-auth-module-check"
                {
                  directoryRulePresent =
                    if builtins.elem
                      "d /var/lib/r9p-session-auth 0700 r9p-proof r9p-proof -"
                      sessionAuthModuleEval.config.systemd.tmpfiles.rules
                    then "1"
                    else "0";
                  privateKeyRulePresent =
                    if builtins.elem
                      "z /var/lib/r9p-session-auth/proof.key 0600 r9p-proof r9p-proof -"
                      sessionAuthModuleEval.config.systemd.tmpfiles.rules
                    then "1"
                    else "0";
                  recursiveOwnerRulePresent =
                    if builtins.elem
                      "Z /var/lib/r9p-session-auth - r9p-proof r9p-proof -"
                      sessionAuthModuleEval.config.systemd.tmpfiles.rules
                    then "1"
                    else "0";
                  publicKeyRulePresent =
                    if builtins.elem
                      "z /var/lib/r9p-session-auth/proof.key.pub 0644 r9p-proof r9p-proof -"
                      sessionAuthModuleEval.config.systemd.tmpfiles.rules
                    then "1"
                    else "0";
                  sharedDirectoryRulePresent =
                    if builtins.elem
                      "d /var/lib/r9p-session-auth-shared 0750 r9p-proof r9p-shared -"
                      sessionAuthModuleEval.config.systemd.tmpfiles.rules
                    then "1"
                    else "0";
                  sharedPrivateKeyRulePresent =
                    if builtins.elem
                      "z /var/lib/r9p-session-auth-shared/proof.key 0640 r9p-proof r9p-shared -"
                      sessionAuthModuleEval.config.systemd.tmpfiles.rules
                    then "1"
                    else "0";
                  executable = service.serviceConfig.ExecStart;
                  packageName = sessionAuthModuleEval.config.services.r9p-session-auth.package.pname;
                  orderedAfterTmpfilesResetup =
                    if builtins.elem
                      "systemd-tmpfiles-resetup.service"
                      service.after
                    then "1"
                    else "0";
                  serviceGroup = service.serviceConfig.Group;
                  serviceUser = service.serviceConfig.User;
                  sharedServiceGroup = sharedService.serviceConfig.Group;
                  sharedServiceUser = sharedService.serviceConfig.User;
                } ''
                test "$packageName" = "r9p"
                test "$orderedAfterTmpfilesResetup" = "1"
                test "$serviceUser" = "r9p-proof"
                test "$serviceGroup" = "r9p-proof"
                test "$directoryRulePresent" = "1"
                test "$privateKeyRulePresent" = "1"
                test "$recursiveOwnerRulePresent" = "1"
                test "$publicKeyRulePresent" = "1"
                test "$sharedDirectoryRulePresent" = "1"
                test "$sharedPrivateKeyRulePresent" = "1"
                test "$sharedServiceUser" = "r9p-proof"
                test "$sharedServiceGroup" = "r9p-shared"
                case "${sharedService.serviceConfig.ExecStart}" in
                  */bin/r9p\ auth-keygen\ --private\ /var/lib/r9p-session-auth-shared/proof.key\ --public\ /var/lib/r9p-session-auth-shared/proof.key.pub\ --private-access\ owner-group-read) ;;
                  *) exit 1 ;;
                esac
                case "$executable" in
                  */bin/r9p\ auth-keygen\ --private\ /var/lib/r9p-session-auth/proof.key\ --public\ /var/lib/r9p-session-auth/proof.key.pub\ --private-access\ owner-only) ;;
                  *) exit 1 ;;
                esac
                touch "$out"
              '';
            native-skill =
              assert self.lib.nativeSkills.r9p.owner == "r9p";
              assert self.lib.nativeSkills.r9p.canonicalName == "r9p";
              assert self.lib.nativeSkills.r9p.source == self.lib.skills.r9p;
              pkgs.runCommandLocal "r9p-native-skill-check" { } ''
                test -f ${self.lib.nativeSkills.r9p.source}/SKILL.md
                grep -F 'name: r9p' ${self.lib.nativeSkills.r9p.source}/SKILL.md >/dev/null
                test ! -e ${r9p.src}/skills
                test ! -e ${r9p.src}/docs
                touch "$out"
              '';
          };

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
