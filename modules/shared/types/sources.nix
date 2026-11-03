{ lib, ... }:
let
  # Secret-bearing paths are `str`, not `path`, on purpose. A `path`
  # option given a path literal is copied into the Nix store, which is
  # world-readable, so typing a private key or a PAT file as `path` is a
  # leak dressed up as type safety. These keys name a file on the
  # running host; xi reads them at runtime and never puts them in a
  # closure (xi-sources-push SC-P-004).
  secretPath =
    description:
    lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        ${description}

        An absolute path on the target host, as a string. Deliberately
        not a `path`: a path literal would copy the secret into the
        world-readable Nix store.
      '';
    };

  # A nullable passthrough key: null means "this module does not pin
  # the key". That is not the same as pinning the documented default -
  # leaving the rung empty lets a CLI flag, an `XI_SOURCES_*` variable
  # or a project `.xi.toml` still answer, and keeps the provenance map
  # saying `default` rather than `nix-module`.
  unpinned =
    {
      type,
      description,
      example ? null,
    }:
    lib.mkOption (
      {
        type = lib.types.nullOr type;
        default = null;
        inherit description;
      }
      // lib.optionalAttrs (example != null) { inherit example; }
    );

  hookOption =
    phase:
    lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "systemctl restart my-service";
      description = ''
        Shell command the gitops daemon runs after ${phase}. A non-zero
        exit does NOT roll back the preceding step; it surfaces as
        `Degraded` on the next `xi sources status` and clears on the
        next successful cycle. stdout and stderr are captured to
        `hooks/<name>.log` in the state dir.
      '';
    };
in
{
  options._xi.types.sources = lib.mkOption {
    type = lib.types.raw;
    internal = true;
    description = "Config-repo sources submodule type (xi sources).";
  };

  # One definition shared by the nixos, home-manager and flake-parts
  # modules. The three surfaces differ only in WHERE the generated
  # sources.toml lands and in which scope assertions apply, never in
  # which keys exist, so the option tree lives here and cannot drift
  # between them. `nix/sources-checks.nix` diffs the three rendered
  # option sets to keep that honest.
  #
  # Option names are camelCase per nixpkgs convention; `mkSourcesFile`
  # maps the subset xi's parser accepts back to kebab-case keys.
  #
  # Only the eight keys in `SOURCES_KEYS`
  # (crates/xi-core/src/sources/config.rs) are rendered today; that key
  # set is closed, so emitting anything else would be a load-time error
  # rather than a silent no-op. The remaining options below are
  # declared here - this tree is the authoritative registry every other
  # xi-sources-* spec amends - and wired by the change that owns them.
  config._xi.types.sources = lib.types.submodule {
    options = {
      enable = lib.mkEnableOption ''
        the xi sources model: a managed clone of your config repo, so
        `xi <scope> switch` works from any directory on the machine
      '';

      remote = unpinned {
        type = lib.types.str;
        example = "git@github.com:you/dotfiles.git";
        description = ''
          Remote git URL of the config repo. Accepts an `https://` or
          `ssh://` URL, git's scp-style `host:path`, or an absolute
          local path.
        '';
      };

      branch = unpinned {
        type = lib.types.str;
        example = "main";
        description = ''
          Branch to track. Leave null to use xi's built-in default
          (`main`), so the CLI flag and `XI_SOURCES_BRANCH` can still
          win without this module pinning a value.
        '';
      };

      workingDir = unpinned {
        type = lib.types.path;
        example = "/home/you/src/dotfiles";
        description = ''
          Absolute path to your editable clone. Recorded so a `switch`
          started from an unrelated directory can still find it.

          User scope only. Setting it at system scope is an eval error:
          a system-wide option cannot name one user's checkout.
        '';
      };

      pollInterval = unpinned {
        type = lib.types.str;
        example = "5m";
        description = ''
          How often the gitops daemon re-checks the remote, as a
          duration string (`60s`, `5m`). Defaults to `60s`.
        '';
      };

      requireSigned = unpinned {
        type = lib.types.bool;
        description = ''
          Refuse to advance past unsigned commits. Requires at least one
          trust root: `allowedSigners`, `gpgPublicKeys`, or both. Only
          the tip commit must be signed.
        '';
      };

      allowedSigners = unpinned {
        type = lib.types.path;
        description = ''
          Absolute path to an SSH `allowed_signers` file. One of this or
          `gpgPublicKeys` is required when `requireSigned` is true:
          verifying against an empty trust root is worse than not
          verifying, because it looks safe.
        '';
      };

      gpgPublicKeys = lib.mkOption {
        type = lib.types.listOf lib.types.path;
        default = [ ];
        example = lib.literalExpression ''[ ./keys/release.asc ]'';
        description = ''
          GPG public key files accepted as signing trust roots, as an
          alternative or a complement to `allowedSigners`. These are
          public keys, so a store copy is harmless.
        '';
      };

      syncMode = unpinned {
        type = lib.types.enum [
          "prompt"
          "auto-copy"
          "never"
        ];
        description = ''
          What to do when the working dir and the sources dir disagree.
          Defaults to `prompt`.
        '';
      };

      gitops = {
        enable = lib.mkEnableOption ''
          the gitops daemon: poll `remote`, fast-forward the sources dir
          and activate on advance.

          Enabling this for the same `remote` at both system and user
          scope is an eval error - the two daemons would race on the
          same clone
        '';

        rebootWindow = lib.mkOption {
          type = lib.types.str;
          default = "always";
          example = "02:00-04:00";
          description = ''
            Daily `HH:MM-HH:MM` window during which the daemon may run
            an activation that requires a reboot. Outside it, such an
            activation is deferred and re-queued for the next window
            start. `always` disables the gate, which is what you want on
            a server and not what you want on a laptop at 3pm.
          '';
        };

        hooks = {
          onAdvance = hookOption "a successful fast-forward of `sources_head`";
          onBuild = hookOption "a successful build";
          onSwitch = hookOption "a successful activation";
        };
      };

      bootstrap = {
        installer = unpinned {
          type = lib.types.enum [
            "detsys"
            "lix"
            "none"
          ];
          description = ''
            Which Nix installer a bootstrap run should use on a machine
            that has no `nix` yet. `detsys` shells out to the
            Determinate Systems installer, `lix` to the Lix fork of it,
            and `none` opts out of the phase entirely.

            Left null so `xi sources bootstrap` picks from the resolved
            flake's substituters: `lix` when the effective substituter
            list includes `cache.lix.systems`, else `detsys`.
          '';
        };
      };

      cachePrestage = {
        enable =
          lib.mkEnableOption ''
            prestaging the declared substituters into `nix.conf` before
            the first build, so a bootstrap does not start by building
            every derivation from source
          ''
          // {
            default = true;
            example = false;
          };

        trustDeclaredKeys = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = ''
            Prestage public keys whose signing fingerprint cannot be
            fetched from the cache over HTTPS. Off by default because a
            declared-but-unverified key is a trust-on-first-use hole;
            turn it on only for airgapped setups, where it is the whole
            point.
          '';
        };
      };

      secrets = lib.mkOption {
        default = { };
        example = lib.literalExpression ''
          {
            ssh-key = {
              source = "/run/agenix/deploy-key";
              destination = "/run/keys/ssh";
            };
          }
        '';
        description = ''
          Files uploaded out-of-band over SSH around activation, keyed
          by a name used in logs and status output. Owned by
          xi-sources-push.

          When the target configuration already declares `agenix` or
          `sops-nix` for the same destination, xi defers to them and
          warns rather than double-declaring.
        '';
        type = lib.types.attrsOf (
          lib.types.submodule {
            options = {
              source = lib.mkOption {
                type = lib.types.str;
                example = "/run/agenix/deploy-key";
                description = ''
                  Absolute path on the operator's machine to read the
                  secret from. A string, not a `path`: a path literal
                  would copy the plaintext into the Nix store, and the
                  whole point of this stanza is that the secret never
                  enters a closure.
                '';
              };

              destination = lib.mkOption {
                type = lib.types.str;
                example = "/run/keys/ssh";
                description = ''
                  Absolute path on the target host to write the secret
                  to. A string for the same reason as `source`.
                '';
              };

              owner = lib.mkOption {
                type = lib.types.str;
                default = "root";
                description = "Owner of the uploaded file on the target.";
              };

              group = lib.mkOption {
                type = lib.types.nullOr lib.types.str;
                default = null;
                description = ''
                  Group of the uploaded file on the target. Null leaves
                  the owner's primary group.
                '';
              };

              mode = lib.mkOption {
                type = lib.types.str;
                default = "0600";
                example = "0440";
                description = "Octal mode of the uploaded file on the target.";
              };

              uploadAt = lib.mkOption {
                type = lib.types.enum [
                  "pre-activate"
                  "post-activate"
                ];
                default = "pre-activate";
                description = ''
                  Whether the upload happens before or after
                  activation. `pre-activate` is the default because a
                  service started by the new generation usually needs
                  its secret to already be there.
                '';
              };
            };
          }
        );
      };

      # Owned by xi-sources-credentials; mirrored here because this tree
      # is the registry. A kind with no field set means "mode disabled";
      # the fallthrough order within a kind lives in that spec.
      credentials = {
        ssh = {
          identityFile = secretPath "Private key offered for `ssh://` and `git@` remotes.";

          useAgent = lib.mkOption {
            type = lib.types.bool;
            default = true;
            description = ''
              Fall back to a running ssh-agent when `identityFile` is
              unset. When `identityFile` IS set, xi composes
              `GIT_SSH_COMMAND` with `IdentitiesOnly=yes`, so the agent
              is not offered at all.
            '';
          };

          knownHostsFile = lib.mkOption {
            type = lib.types.nullOr lib.types.path;
            default = null;
            description = ''
              Override for the `UserKnownHostsFile` passed to ssh. Host
              public keys, so a store copy is harmless.
            '';
          };

          strictHostKeyChecking = lib.mkOption {
            type = lib.types.enum [
              "yes"
              "no"
              "accept-new"
            ];
            default = "yes";
            description = ''
              `StrictHostKeyChecking` policy for git's ssh transport.
              `yes` outside bootstrap; a first-run bootstrap may lower
              it to `accept-new` for its own clone only.
            '';
          };
        };

        https = {
          tokenFile = secretPath ''
            File containing a personal access token on a single line,
            used for `https://` remotes.
          '';

          credentialHelper = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "!gh auth git-credential";
            description = ''
              git credential helper invocation, as an alternative to
              `tokenFile`.
            '';
          };

          username = lib.mkOption {
            type = lib.types.str;
            default = "git";
            description = ''
              Username paired with the token. `git` is what PAT auth
              wants on every forge that matters.
            '';
          };
        };

        oauth = {
          askpass = lib.mkOption {
            type = lib.types.nullOr lib.types.path;
            default = null;
            description = ''
              Executable that prints a token on stdout, mirroring git's
              `GIT_ASKPASS`. This kind has no fallthrough: if askpass
              fails, the fetch fails.
            '';
          };
        };

        anonymous = {
          allow = lib.mkOption {
            type = lib.types.bool;
            default = true;
            description = ''
              Allow an unauthenticated attempt as the last rung of the
              fallthrough. Set false for a repo that MUST be private, so
              a missing credential fails loudly instead of degrading
              into a 404.
            '';
          };
        };
      };
    };
  };
}
