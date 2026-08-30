{
  description = "Reusable Rust 9P2000 protocol primitives";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    cargo-nix-plugin = {
      url = "git+ssh://git@git.mesh:2222/MrVampy/cargo-nix-plugin.git?ref=main";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, cargo-nix-plugin }:
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
          workspaceSource = nixpkgs.lib.fileset.toSource {
            root = ./.;
            fileset = nixpkgs.lib.fileset.unions [
              ./.cargo
              ./Cargo.lock
              ./Cargo.toml
              ./crates
            ];
          };
          cargoNixPluginSource = cargo-nix-plugin.outPath or cargo-nix-plugin;
          cargoNix = import "${cargoNixPluginSource}/lib" {
            inherit pkgs;
            src = workspaceSource;
            clippyArgs = [
              "-D"
              "warnings"
            ];
            crateOverrides = pkgs.defaultCrateOverrides // {
              cli = _: {
                nativeCheckInputs = [ pkgs.git ];
              };
            };
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
          cliMember = cargoNix.workspaceMembers.cli;
          r9p = pkgs.symlinkJoin {
            name = "r9p-0.1.0";
            pname = "r9p";
            version = "0.1.0";
            paths = [ cliMember.build ];
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postBuild = ''
              wrapProgram "$out/bin/r9p" \
                --suffix PATH : ${pkgs.lib.makeBinPath [ pkgs.fuse3 ]}
            '';
            passthru = {
              src = cliMember.build.src;
              cargoTests = cliMember.runTests;
              cargoClippy = cargoNix.clippy.workspaceMembers.cli.checkTests;
            };
            meta = {
              description = "Reusable 9P2000 command-line and FUSE client";
              license = pkgs.lib.licenses.mit;
              mainProgram = "r9p";
              platforms = pkgs.lib.platforms.unix;
            };
          };
          frontMember = cargoNix.workspaceMembers.front;
          frontAssets = pkgs.runCommandLocal "r9p-front-assets-abi23" { } ''
              install -Dm644 ${./crates/front/include/r9p_front.h} \
                "$out/include/r9p_front.h"
              install -Dm644 ${./crates/front/bindings/deno/front_sink.ts} \
                "$out/share/r9p/front/deno/front_sink.ts"
              install -Dm644 ${./crates/front/bindings/deno/export_descriptor.ts} \
                "$out/share/r9p/front/deno/export_descriptor.ts"
              install -Dm644 ${./crates/front/bindings/deno/request_context.ts} \
                "$out/share/r9p/front/deno/request_context.ts"
          '';
          front = pkgs.symlinkJoin {
            name = "r9p-front-0.1.0-abi23";
            paths = [ frontMember.build frontAssets ];
            passthru = {
              cargoTests = frontMember.runTests;
              cargoClippy = cargoNix.clippy.workspaceMembers.front.checkTests;
            };
            meta = {
              description = "Native r9p Front ABI library";
              license = pkgs.lib.licenses.mit;
              platforms = pkgs.lib.platforms.unix;
            };
          };
          beamPortMember = cargoNix.workspaceMembers."beam-port";
          beamPort = beamPortMember.build;
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
          frontTests = pkgs.linkFarmFromDrvs "r9p-front-tests" [
            frontMember.runTests
            beamPortMember.runTests
          ];
          cargoMemberTests = pkgs.lib.mapAttrs' (
            name: member: pkgs.lib.nameValuePair "cargo-${name}-tests" member.runTests
          ) cargoNix.workspaceMembers;
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

          checks = pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux ({
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
          } // cargoMemberTests);

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
