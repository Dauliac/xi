{
  config,
  lib,
  ...
}:
let
  cfg = config.services.xiVmGuest;

  # The fixed, impure path the Rust `xi-vm` crate ensures a keypair
  # exists at before invoking `nix build` on the `--dev` variant (see
  # `crates/xi-vm/src/ssh_key.rs`). Deliberately a string read outside
  # the flake rather than a flake-relative `path`: the key is generated
  # at runtime by the operator's `xi` invocation, not committed, and a
  # `path` literal would require it to already exist at eval time for a
  # flake that has never built a VM yet.
  keyPath = "${toString cfg.stateDir}/ssh/id_ed25519.pub";
  keyExists = builtins.pathExists keyPath;
in
{
  options.services.xiVmGuest = {
    enable = lib.mkEnableOption ''
      the xi-vm-lifecycle guest account: a dedicated login user with an
      xi-managed authorized key, for `xi vm`'s `--dev` build variant only
    '';

    sshUser = lib.mkOption {
      type = lib.types.str;
      default = "xi";
      description = "Login account to create.";
    };

    stateDir = lib.mkOption {
      type = lib.types.path;
      description = ''
        The invoking operator's xi state dir, as an impure absolute path
        (not copied into the store as a derivation input - only
        `<stateDir>/ssh/id_ed25519.pub` is read, via `builtins.readFile`).
      '';
    };

    passwordlessSudo = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Grant `sshUser` passwordless sudo, scoped to that one user via
        `security.sudo.extraRules`. Never sets `wheelNeedsPassword`.
      '';
    };

    sshUserAlreadyDeclared = lib.mkOption {
      type = lib.types.bool;
      default = false;
      internal = true;
      description = ''
        Whether `sshUser` was already declared on the *pre-extension*
        configuration (computed by `modules/flake-parts/vm.nix`, which
        alone can see both the plain and the extended config). Checking
        `config.users.users` directly from inside this module would
        always read `true`, since by evaluation time this module's own
        `users.users.''${sshUser}` definition below is already part of
        that same merged `config` - there would be no way to tell "the
        target already had this user" apart from "I just declared it".
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !cfg.sshUserAlreadyDeclared;
        message = ''
          xi vm: this configuration already declares a user named
          '${cfg.sshUser}'. xi-vm-lifecycle creates a dedicated guest
          account and refuses to merge into an existing one - set
          `xi.vm.<name>.sshUser` to a name this configuration does not
          already use.
        '';
      }
      {
        assertion = keyExists;
        message = ''
          xi vm: no SSH public key found at ${keyPath}. This is normally
          generated automatically by `xi vm build`/`xi vm start` before
          invoking this build; run one of those instead of building this
          attribute directly, or pre-generate the keypair yourself.
        '';
      }
    ];

    users.users.${cfg.sshUser} = {
      isNormalUser = true;
      openssh.authorizedKeys.keys = lib.optional keyExists (lib.fileContents keyPath);
    };

    services.openssh.enable = lib.mkDefault true;

    security.sudo.extraRules = lib.mkDefault (
      lib.optional cfg.passwordlessSudo {
        users = [ cfg.sshUser ];
        commands = [
          {
            command = "ALL";
            options = [ "NOPASSWD" ];
          }
        ];
      }
    );
  };
}
