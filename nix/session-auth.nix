{ config
, defaultPackage
, lib
, ...
}:

let
  cfg = config.services.r9p-session-auth;
  keyType = lib.types.submodule ({ name, ... }: {
    options = {
      privateKeyFile = lib.mkOption {
        type = lib.types.str;
        description = "Absolute path for the private Noise static key.";
      };

      publicKeyFile = lib.mkOption {
        type = lib.types.str;
        description = "Absolute path for the public Noise static key.";
      };

      user = lib.mkOption {
        type = lib.types.str;
        default = "root";
        description = "User that owns and generates the key pair.";
      };

      group = lib.mkOption {
        type = lib.types.str;
        default = "root";
        description = "Group that owns the key-pair directory.";
      };

      directoryMode = lib.mkOption {
        type = lib.types.strMatching "[0-7]{4}";
        default = "0700";
        description = "Mode for the private key-pair directory.";
      };
    };
  });

  absoluteWithoutWhitespace = path:
    lib.hasPrefix "/" path && builtins.match ".*[[:space:]].*" path == null;

  keyEntries = lib.mapAttrsToList
    (name: key: {
      inherit name key;
      directory = builtins.dirOf key.privateKeyFile;
      directoryRule =
        "d ${builtins.dirOf key.privateKeyFile} ${key.directoryMode} ${key.user} ${key.group} -";
    })
    cfg.keys;

  directoryGroups = lib.groupBy (entry: entry.directory) keyEntries;

  keyAssertions = (lib.concatMap
    (name:
      let
        key = cfg.keys.${name};
      in
      [
        {
          assertion = builtins.match "[A-Za-z0-9_-]+" name != null;
          message = "services.r9p-session-auth.keys names must be valid systemd unit-name fragments";
        }
        {
          assertion = absoluteWithoutWhitespace key.privateKeyFile;
          message = "services.r9p-session-auth.keys.${name}.privateKeyFile must be an absolute path without whitespace";
        }
        {
          assertion = absoluteWithoutWhitespace key.publicKeyFile;
          message = "services.r9p-session-auth.keys.${name}.publicKeyFile must be an absolute path without whitespace";
        }
        {
          assertion = key.privateKeyFile != key.publicKeyFile;
          message = "services.r9p-session-auth.keys.${name} must use different private and public key paths";
        }
        {
          assertion = builtins.dirOf key.privateKeyFile == builtins.dirOf key.publicKeyFile;
          message = "services.r9p-session-auth.keys.${name} private and public keys must share a directory";
        }
      ])
    (lib.attrNames cfg.keys)) ++ (lib.mapAttrsToList
    (directory: entries:
      let
        expectedRule = (builtins.head entries).directoryRule;
      in
      {
        assertion = lib.all (entry: entry.directoryRule == expectedRule) entries;
        message = "r9p session keys sharing ${directory} must use the same owner, group, and directory mode";
      })
    directoryGroups) ++ [
    {
      assertion =
        let
          paths = lib.concatMap
            (entry: [ entry.key.privateKeyFile entry.key.publicKeyFile ])
            keyEntries;
        in
        builtins.length paths == builtins.length (lib.unique paths);
      message = "services.r9p-session-auth.keys must use unique private and public key paths";
    }
  ];

  keyDirectories = lib.concatLists (lib.mapAttrsToList
    (directory: entries:
      let
        entry = builtins.head entries;
      in
      [
        entry.directoryRule
        "Z ${directory} - ${entry.key.user} ${entry.key.group} -"
      ])
    directoryGroups);
  keyFiles = lib.concatMap
    (entry: [
      "z ${entry.key.privateKeyFile} 0600 ${entry.key.user} ${entry.key.group} -"
      "z ${entry.key.publicKeyFile} 0644 ${entry.key.user} ${entry.key.group} -"
    ])
    keyEntries;

  keyServices = lib.mapAttrs'
    (name: key:
      lib.nameValuePair "r9p-session-key-${name}" {
        description = "Provision and verify r9p session key ${name}";
        wantedBy = [ "multi-user.target" ];
        serviceConfig = {
          Type = "oneshot";
          User = key.user;
          Group = key.group;
          UMask = "0077";
          ExecStart = lib.escapeShellArgs [
            "${cfg.package}/bin/r9p"
            "auth-keygen"
            "--private"
            key.privateKeyFile
            "--public"
            key.publicKeyFile
          ];
          RemainAfterExit = true;
        };
      })
    cfg.keys;
in
{
  options.services.r9p-session-auth = {
    package = lib.mkOption {
      type = lib.types.package;
      default = defaultPackage;
      defaultText = lib.literalExpression "inputs.r9p.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "r9p package that provides auth-keygen.";
    };

    keys = lib.mkOption {
      type = lib.types.attrsOf keyType;
      default = { };
      description = "Static Noise key pairs provisioned by r9p auth-keygen.";
    };
  };

  config = lib.mkIf (cfg.keys != { }) {
    assertions = keyAssertions;
    systemd.tmpfiles.rules = keyDirectories ++ keyFiles;
    systemd.services = keyServices;
  };
}
