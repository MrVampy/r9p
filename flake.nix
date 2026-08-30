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
          cargoNixPluginSource = cargo-nix-plugin.outPath or cargo-nix-plugin;
          mkCargoNix =
            src:
            import "${cargoNixPluginSource}/lib" {
              inherit pkgs src;
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
          cargoNix = mkCargoNix ./.;
          sourceWithout = excluded: builtins.path {
            path = ./.;
            name = "source";
            filter =
              path: _type:
              let
                relative = nixpkgs.lib.removePrefix (toString ./. + "/") (toString path);
              in
              relative != excluded;
          };
          fuseEditedCargoNix = mkCargoNix (sourceWithout "crates/fuse/src/fuse/read_cache.rs");
          mountCliEditedCargoNix =
            mkCargoNix (sourceWithout "crates/mount-cli/src/direct/config.rs");
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
          mkR9p = cargo:
            let
              member = cargo.workspaceMembers.cli;
            in
            pkgs.symlinkJoin {
              name = "r9p-0.1.0";
              pname = "r9p";
              version = "0.1.0";
              paths = [ member.build ];
              passthru = {
                src = member.build.src;
                cargoTests = member.runTests;
                cargoClippy = cargo.clippy.workspaceMembers.cli.checkTests;
              };
              meta = {
                description = "Reusable 9P2000 command-line client";
                license = pkgs.lib.licenses.mit;
                mainProgram = "r9p";
                platforms = pkgs.lib.platforms.unix;
              };
            };
          r9p = mkR9p cargoNix;
          fuseEditedR9p = mkR9p fuseEditedCargoNix;
          mountCliEditedR9p = mkR9p mountCliEditedCargoNix;
          mkMountHelper = cargo:
            let
              member = cargo.workspaceMembers."mount-cli";
            in
            pkgs.runCommandLocal "r9p-mount-helper-0.1.0"
              {
                nativeBuildInputs = [ pkgs.makeWrapper ];
                passthru = {
                  src = member.build.src;
                  cargoTests = member.runTests;
                  cargoClippy = cargo.clippy.workspaceMembers."mount-cli".checkTests;
                };
              }
              ''
                mkdir -p "$out/bin"
                makeWrapper ${member.build}/bin/r9p-mount "$out/bin/r9p-mount" \
                  --suffix PATH : ${pkgs.lib.makeBinPath [ pkgs.fuse3 ]}
              '';
          mountHelper = mkMountHelper cargoNix;
          fuseEditedMountHelper = mkMountHelper fuseEditedCargoNix;
          mountCliEditedMountHelper = mkMountHelper mountCliEditedCargoNix;
          mkWithMount = client: helper: pkgs.symlinkJoin {
            name = "r9p-with-mount-0.1.0";
            paths = [ client helper ];
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postBuild = ''
              wrapProgram "$out/bin/r9p" \
                --set R9P_MOUNT_HELPER ${helper}/bin/r9p-mount
            '';
            passthru = {
              inherit client helper;
            };
            meta = {
              description = "r9p client with the FUSE mount command adapter";
              license = pkgs.lib.licenses.mit;
              mainProgram = "r9p";
              platforms = pkgs.lib.platforms.unix;
            };
          };
          withMount = mkWithMount r9p mountHelper;
          fuseEditedWithMount = mkWithMount fuseEditedR9p fuseEditedMountHelper;
          mountCliEditedWithMount = mkWithMount mountCliEditedR9p mountCliEditedMountHelper;
          cliMember = cargoNix.workspaceMembers.cli;
          frontMember = cargoNix.workspaceMembers.front;
          fuseEditedFrontMember = fuseEditedCargoNix.workspaceMembers.front;
          mountCliEditedFrontMember = mountCliEditedCargoNix.workspaceMembers.front;
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
          mkFront = cargo: member: pkgs.symlinkJoin {
            name = "r9p-front-0.1.0-abi23";
            paths = [ (pkgs.lib.getLib member.build) frontAssets ];
            passthru = {
              cargoTests = member.runTests;
              cargoClippy = cargo.clippy.workspaceMembers.front.checkTests;
            };
            meta = {
              description = "Native r9p Front ABI library";
              license = pkgs.lib.licenses.mit;
              platforms = pkgs.lib.platforms.unix;
            };
          };
          front = mkFront cargoNix frontMember;
          fuseEditedFront = mkFront fuseEditedCargoNix fuseEditedFrontMember;
          mountCliEditedFront = mkFront mountCliEditedCargoNix mountCliEditedFrontMember;
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
          packages.mount-helper = mountHelper;
          packages.r9p = r9p;
          packages.with-mount = withMount;

          checks = pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux ({
            r9p = r9p;
            beam-front = frontTests;
            front-package = pkgs.runCommandLocal "r9p-front-package-check" { } ''
              test -e ${front}/lib/libfront.so
              test -e ${front}/include/r9p_front.h
              test -e ${front}/share/r9p/front/deno/front_sink.ts
              touch "$out"
            '';
            product-source-isolation =
              assert builtins.pathExists ./crates/fuse/src/fuse/read_cache.rs;
              assert builtins.pathExists ./crates/mount-cli/src/direct/config.rs;
              assert toString frontMember.build.src == toString fuseEditedFrontMember.build.src;
              assert
                toString frontMember.build.src == toString mountCliEditedFrontMember.build.src;
              assert front.drvPath == fuseEditedFront.drvPath;
              assert front.drvPath == mountCliEditedFront.drvPath;
              assert toString r9p.src == toString fuseEditedR9p.src;
              assert toString r9p.src == toString mountCliEditedR9p.src;
              assert r9p.drvPath == fuseEditedR9p.drvPath;
              assert r9p.drvPath == mountCliEditedR9p.drvPath;
              assert withMount.drvPath != fuseEditedWithMount.drvPath;
              assert withMount.drvPath != mountCliEditedWithMount.drvPath;
              assert builtins.all
                (dependency: dependency.name != "fuse")
                cliMember.crateInfo.dependencies;
              assert builtins.any
                (dependency: dependency.name == "fuse")
                cargoNix.workspaceMembers."mount-cli".crateInfo.dependencies;
              assert builtins.any
                (dependency: dependency.name == "cli")
                cargoNix.workspaceMembers."mount-cli".crateInfo.dependencies;
              pkgs.runCommandLocal "r9p-product-source-isolation-check"
                {
                  frontSource = frontMember.build.src;
                  clientSource = r9p.src;
                  mountSource = mountHelper.src;
                }
                ''
                  test -f "$frontSource/crates/front/Cargo.toml"
                  test ! -e "$frontSource/crates/fuse"
                  test ! -e "$frontSource/crates/cli"
                  test -f "$clientSource/crates/cli/Cargo.toml"
                  test ! -e "$clientSource/crates/fuse"
                  test ! -e "$clientSource/crates/mount-cli"
                  test -f "$mountSource/crates/mount-cli/Cargo.toml"
                  test ! -e "$mountSource/crates/cli"
                  test ! -e "$mountSource/crates/fuse"
                  touch "$out"
                '';
            product-contents = pkgs.runCommandLocal "r9p-product-contents-check" { } ''
              test -x ${r9p}/bin/r9p
              test ! -e ${r9p}/bin/r9p-mount
              test -x ${mountHelper}/bin/r9p-mount
              test ! -e ${mountHelper}/bin/r9p
              test -x ${withMount}/bin/r9p
              test -x ${withMount}/bin/r9p-mount
              touch "$out"
            '';
            fuse-runtime-helper = pkgs.runCommandLocal "r9p-fuse-runtime-helper-check" { } ''
              grep -F ${pkgs.lib.escapeShellArg "${pkgs.fuse3}/bin"} ${mountHelper}/bin/r9p-mount
              grep -F 'R9P_MOUNT_HELPER' ${withMount}/bin/r9p
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
