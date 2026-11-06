# Nix / NixOS secrets management: state of the art, 2025-2026

Research date: **2026-11-04**. All repository metrics in this document were read
from `api.github.com` on that date, and every mechanism claim is cited to the
tool's own README, docs directory, or module source. Secondary sources are used
only in the "Community sentiment" section, and are labelled as such.

## Scope

The question: what should someone building or operating NixOS systems use to
keep secrets out of the world-readable Nix store in 2026? Four tools are
examined in depth (sops-nix, agenix, agenix-rekey, ragenix), plus the adjacent
ecosystem that is actually in use (clan.lol `vars`, cachix `secretspec`, `vals`,
systemd credentials).

The one security property that matters more than any feature comparison: **every
path in `/nix/store` is world-readable**, so plaintext must never enter it. This
document checks that property per tool, at both build time and activation time,
rather than assuming it.

A note on terminology, because it is the single most misread thing in this
space: all four tools put the _encrypted_ file in the Nix store and decrypt at
_activation_ time. Encrypted-in-store is the design, not a leak.

---

## Comparison table

| Tool                | Key type                                                                                                              | Decryption timing                                                                  | HM support                                                             | Multi-host model                                                                            | CI/CD story                                                                                                | Stars        | Last commit (default branch) | Maintenance                                                       |
| ------------------- | --------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- | ------------ | ---------------------------- | ----------------------------------------------------------------- |
| **sops-nix**        | age + PGP/GnuPG; SSH Ed25519 via `ssh-to-age`, SSH RSA via `ssh-to-pgp`; cloud KMS "not officially supported ... yet" | Activation. Activation script, or a systemd unit when `useSystemdActivation`       | Yes, official `homeManagerModules.sops`; explicitly a feature subset   | `.sops.yaml` `creation_rules` with `path_regex` + `key_groups`, YAML anchors per host/admin | Strong. Build needs no secrets; decryption via `SOPS_AGE_KEY` / `_FILE` / `_CMD` / `_SSH_PRIVATE_KEY_FILE` | 3254         | 2026-11-02                   | **Very active.** No semver releases (rolling `master`)            |
| **agenix**          | age only; recipients are SSH pubkeys (`ssh-ed25519`, `ssh-rsa`) or age keys. "No GPG"                                 | Activation. Activation script, or systemd units under `sysusers`/`userborn`        | Yes, `modules/age-home.nix`; no ramfs, no chown                        | `agenix-rules.nix` (legacy `secrets.nix`) listing `publicKeys` per path                     | Workable but manual. Host SSH key is the identity, so CI needs a key handed to it                          | 2533         | 2026-10-20                   | **Active.** 0.16.0 / 0.17.0 / 0.18.0 all released Sept 2026       |
| **agenix-rekey**    | Master identity (YubiKey / FIDO2 / TPM / age) re-encrypted to per-host pubkeys; age plugins                           | Rekey is an explicit out-of-band step; runtime decryption is agenix's (activation) | **No HM module** (only a NixOS/darwin module and a flake-parts module) | Host-key inference from `hostPubkey`; no rules file to maintain                             | Mode-dependent. `local` works in CI; `derivation` explicitly does not                                      | 448          | 2026-10-09                   | **Active.** No releases (rolling)                                 |
| **ragenix**         | Same as agenix (it re-exports agenix's modules)                                                                       | Same as agenix                                                                     | Inherited from agenix                                                  | Same `secrets.nix` rules model                                                              | Same as agenix; `nix flake check` needs `recursive-nix`                                                    | 483          | **2025-10-30**               | **Dormant.** See the caveat below                                 |
| clan.lol `vars`     | Pluggable `secretStore`: sops (default), age, password-store, custom                                                  | Activation; secrets land in `/run/secrets/`                                        | Clan-scoped, not a standalone HM module                                | Per-machine, managed by clan                                                                | Clan-native                                                                                                | 181 (mirror) | 2026-10-26                   | Active; stars understate adoption (canonical repo is self-hosted) |
| cachix `secretspec` | ~35 providers (keyring, 1Password, Vault/OpenBao, AWS/GCP/Azure, age, SOPS, k8s, systemd creds)                       | Resolve-and-reconcile; delivery via `LoadCredentialEncrypted=`                     | n/a                                                                    | Provider profiles per environment                                                           | "One contract. Development, CI, and production."                                                           | 1634         | 2026-11-04                   | Active; **NixOS integration is emerging, not settled**            |
| `vals`              | `ref+`/`secretref+` URI substitution against Vault, AWS SSM/SM/S3/KMS, GCP, Azure                                     | Render time, outside Nix                                                           | n/a                                                                    | n/a                                                                                         | Pipeline-native                                                                                            | n/a          | n/a                          | Active, but not a NixOS activation mechanism                      |

### Caveat on the ragenix date

The GitHub API reports `pushed_at: 2026-11-01T02:07:27Z` for `yaxitech/ragenix`,
which looks healthy. It is not. `pushed_at` counts pushes to _any_ branch, and
the repo's branch list is `main`, `update-flake`, `age-passphrase-identity`, and
`dependabot/cargo/rsa-0.9.10`. The commit at `main` is dated **2025-10-30**, and
the commits before it are 2025-04-17, 2025-03-09, then 2024-09-19. The 2025
commits are flake-input bumps ("Update flake inputs to fix build on newer
nixpkgs"), not features. The repo has **zero GitHub releases** and 6 open PRs.
Anyone reading `pushed_at` alone would conclude the opposite of the truth. This
is exactly the kind of number worth not guessing.

---

## sops-nix (Mic92/sops-nix)

Repo metrics, `api.github.com`, 2026-11-04: 3254 stars, 239 forks, 111 open
issues, created 2020-07-06, `pushed_at` 2026-11-02T22:16:38Z, not archived, MIT.
Latest commits on `master` are 2026-11-02, so the project is being worked on in
the days before this document was written. Note there are no semver releases:
the only tag is `assets`, published 2021-08-29, so consumers pin a flake
revision rather than a version.

### Mechanism

The README's own framing: "Secrets are decrypted from
[`sops` files](https://github.com/mozilla/sops#2usage) during activation time."
([README](https://github.com/Mic92/sops-nix/blob/master/README.md))

What is encrypted is a sops document (YAML, JSON, env, ini, or binary), and the
recipients are age or PGP keys. Key-material plumbing:

- `ssh-to-age`: "The tool ([`ssh-to-age`](https://github.com/Mic92/ssh-to-age))
  can convert SSH host or user keys in Ed25519 format to `age` keys."
- `ssh-to-pgp`: "can be used to derive a GPG key from a SSH (host) key in RSA
  format", with the caveat "ssh-to-pgp only supports RSA keys; to use Ed25519
  keys, use `age`".
- Cloud KMS: "Sops also supports cloud key management APIs such as AWS KMS, GCP
  KMS, Azure Key Vault and Hashicorp Vault. While not officially supported by
  sops-nix yet, these can be controlled using environment variables."

**Store safety.** The README states the build-time property directly: "Since
sops files can be added to the Nix store without leaking secrets, a machine
definition can be built as a whole from a repository, without needing to rely on
external secrets or services." The module also enforces this at the type level:
`sops.age.keyFile` has type `pathNotInStore`
([`modules/sops/default.nix`](https://github.com/Mic92/sops-nix/blob/master/modules/sops/default.nix)),
so the module refuses a key path that lives in the store. That is a real guard,
not just documentation.

### Where plaintext lands

`/run/secrets`, which the README describes as "a symlink to
`/run/secrets.d/{number}`". The numbered generations give atomic swap on
rebuild. Permissions from the README: parent `/run/secrets.d` is "only owned by
`root` and the `keys` group has read access to it"; nested keys create
directories "owned by root:keys and have permissions 0751"; individual secrets
default to `-r-------- 1 root root`.

Secrets marked `neededForUsers = true` are decrypted to `/run/secrets-for-users`
before user creation, which is what makes `users.users.<n>.hashedPasswordFile`
work.

**Backing filesystem: ramfs by default.** `useTmpfs` defaults to `false`, and
the option description explains why opting in is a downgrade:

> Use tmpfs in place of ramfs for secrets storage. _WARNING_ On Linux kernels
> earlier than 6.4, enabling this option has the potential to write secrets to
> disk unencrypted if the tmpfs volume is written to swap. Do not use unless
> absolutely necessary.

([`modules/sops/default.nix`](https://github.com/Mic92/sops-nix/blob/master/modules/sops/default.nix))

### Activation ordering

Two code paths, selected by `useSystemdActivation`:

> Use a systemd unit to install secrets, instead of deploying them using an
> activation script. This option is automatically enabled when systemd-sysusers
> or userborn are used to manage users and groups. It can also be useful to
> specify additional dependencies to be satisfied before secrets are installed,
> such as required mountpoints for SOPS key files.

Its default is
`(options.systemd ? sysusers && config.systemd.sysusers.enable) || (options.services ? userborn && config.services.userborn.enable)`.
When enabled, `systemd.services.sops-install-secrets` is ordered before
`systemd-sysusers.service` and `userborn.service`; otherwise
`system.activationScripts.setupSecrets` is used. The reason is given in a source
comment: "When using sysusers we no longer are started as an activation script
because those are started in initrd while sysusers is started later."

### Service integration

sops-nix has two things agenix lacks. First, per-secret restart and reload
triggers: `sops.secrets.<name>.restartUnits` and `.reloadUnits`, documented in
the module as working "the same way as
`systemd.services.<name>.restartTriggers`". Second, templates, for the common
case where a service wants a config file rather than a bare secret file:

```nix
sops.templates."your-config-with-secrets.toml".content = ''
  password = "${config.sops.placeholder.your-secret}"
'';
sops.templates."your-config-with-secrets.toml".owner = "serviceuser";
```

There is **no native `LoadCredential` option**. The documented pattern is to
point `LoadCredential=` (or `ExecStart`) at `config.sops.secrets.<n>.path` or a
template path yourself.

### Key rotation

Per-file and manual:

```bash
nix-shell -p sops --run "sops updatekeys secrets/example.yaml"
```

The README is explicit that this is a per-file chore after a recipient change:
"If you add a new host to your `.sops.yaml` file, you will need to update the
keys for all secrets that are used by the new host." There is no fleet-wide
rotation verb, and no rotation policy guidance. You must be able to decrypt a
file to update its keys.

### Multi-host scoping

`.sops.yaml` `creation_rules`, matching on `path_regex` and selecting
`key_groups` (separate `pgp:` and `age:` lists), with YAML anchors naming hosts
and admins:

```yaml
keys:
  - &admin_alice 2504791468b153b8a3963cc97ba53d1919c5dfd4
  - &admin_bob age12zlz6lvcdk6eqaewfylg35w0syh58sm7gh53q5vvn7hd7c6nngyseftjxl
  - &server_azmidi 0fd60c8c3b664aceb1796ce02b318df330331003
creation_rules:
  - path_regex: secrets/azmidi/[^/]+\.(yaml|json|env|ini)$
    key_groups:
      - pgp:
          - *admin_alice
          - *server_azmidi
```

Per-host directories fall out of `path_regex`. Mixing age and PGP recipients in
one group is supported, which matters for fleets with a mix of modern and legacy
hosts. Host pubkeys can be harvested without touching the host's filesystem:
`ssh-keyscan example.com | ssh-to-age`.

### Home-manager

Imported as `inputs.sops-nix.homeManagerModules.sops`. The README is unusually
candid that this is a reduced-security deployment, and the reason is precisely
the one that matters: **home-manager activation runs as the user, not root.**

> The home-manager module provides a subset of features provided by the
> system-wide sops-nix since features like the creation of the ramfs and
> changing the owner of the secrets are not available for non-root users.

Consequences, all from the README and
[`modules/home-manager/sops.nix`](https://github.com/Mic92/sops-nix/blob/master/modules/home-manager/sops.nix):

- Secrets go to `$XDG_RUNTIME_DIR/secrets.d` (option default `"%r/secrets.d"`,
  where "`%r` is replaced by `$XDG_RUNTIME_DIR` on linux"), and are symlinked to
  `$HOME/.config/sops-nix/secrets`. No ramfs is mounted; you inherit whatever
  `$XDG_RUNTIME_DIR` already is.
- **Timing is a systemd user service, not an activation script**, and ordering
  is your job: "The secrets are decrypted in a systemd user service called
  `sops-nix`, so other services needing secrets must order after it:
  `systemd.user.services.mbsync.Unit.After = [ "sops-nix.service" ];`" The
  module defines `systemd.user.services.sops-nix` with
  `Description = "sops-nix activation"`.
- "requires systemd/user as it runs a service called `sops-nix.service` rather
  than an activation script".
- `home.homeDirectory` must be set, "to determine the home-directory on
  evaluation".
- The identity must be unattended: "It's also possible to use a ssh key, but
  only when it has no password."
- macOS uses a launchd agent, logging to `~/Library/Logs/SopsNix/`.

### CI/CD

The build side is clean: encrypted files are committed, so a CI runner can
evaluate and build a full system closure with no secret access at all. For the
cases where CI genuinely must decrypt, the identity sources come from sops
itself, read here from
[`age/keysource.go`](https://github.com/getsops/sops/blob/main/age/keysource.go):
`SOPS_AGE_KEY` (key material inline), `SOPS_AGE_KEY_FILE` (path),
`SOPS_AGE_KEY_CMD` (command that prints the key), and
`SOPS_AGE_SSH_PRIVATE_KEY_FILE`. The usual pattern is a dedicated CI age
identity added as a recipient in `creation_rules`, injected as a masked CI
variable, which keeps host keys out of the runner.

---

## agenix (ryantm/agenix)

Repo metrics, 2026-11-04: 2533 stars, 159 forks, 115 open issues, created
2020-09-01, `pushed_at` 2026-10-20T14:36:57Z, not archived, CC0-1.0. Release
cadence is worth noting: **0.16.0, 0.17.0 and 0.18.0 were all published in
September 2026** (2026-09-26, 2026-09-26, 2026-09-27), after a long quiet
period. The project is clearly being maintained again.

### Mechanism

From
[`doc/features.md`](https://github.com/ryantm/agenix/blob/main/doc/features.md):

> - Secrets are encrypted with SSH keys
>   - system public keys via `ssh-keyscan`
>   - can use public keys available on GitHub for users
> - No GPG
> - Very little code, so it should be easy for you to audit
> - Encrypted secrets are stored in the Nix store, so a separate distribution
>   mechanism is not necessary

And from
[`doc/problem-and-solution.md`](https://github.com/ryantm/agenix/blob/main/doc/problem-and-solution.md):

> All files in the Nix store are readable by any system user, so it is not a
> suitable place for including cleartext secrets. [...] `agenix` solves these
> issues by using your pre-existing SSH key infrastructure and `age` to encrypt
> secrets into the Nix store. Secrets are decrypted using an SSH host private
> key during NixOS system activation.

**Rules file rename.** The rules file is now `agenix-rules.nix`; `secrets.nix`
is still accepted but only in the current directory.
[`pkgs/agenix.sh`](https://github.com/ryantm/agenix/blob/main/pkgs/agenix.sh)
implements the search as: "Searches the current directory for
`agenix-rules.nix`, then `secrets.nix`. Searches parent directories for
`agenix-rules.nix` only." `AGENIX_RULES` (or legacy `RULES`) overrides. This
matters for the ragenix compatibility discussion below.

### Where plaintext lands

From
[`modules/age.nix`](https://github.com/ryantm/agenix/blob/main/modules/age.nix):
`age.secretsDir` defaults to `/run/agenix` and is a symlink;
`age.secretsMountPoint` defaults to `/run/agenix.d` and holds numbered
generations, with the previous generation `rm -rf`'d after relinking. Per-secret
`mode` defaults to `"0400"` and `owner` to `"0"`.

**agenix mounts its own filesystem**, which sops-nix's home-manager module
cannot and which sops-nix's NixOS module does with ramfs. The selection logic in
`modules/age.nix` is three-way:

- tmpfs with `nodev,nosuid,mode=0751,noswap` when the kernel is at least 6.4
  (`noswap` is what makes tmpfs safe here);
- ramfs with `nodev,nosuid,mode=0751` on older kernels;
- ramfs inside containers, with the reason in a source comment: "tmpfs in
  containers may not support turning off swap: 'Turning off swap in unprivileged
  tmpfs mounts unsupported'", citing
  [issue #455](https://github.com/ryantm/agenix/issues/455);
- on Darwin, an `hdiutil` RAM disk formatted HFS and mounted
  `nobrowse,nodev,nosuid`.

Decryption runs with `umask u=r,g=,o=` into a `.tmp` file which is `chmod`'d and
then `mv -f`'d into place, so the plaintext is never briefly world-readable.

### Activation ordering

The same sysusers split as sops-nix, and for the same reason (source comment:
"When using sysusers we no longer be started as an activation script because
those are started in initrd while sysusers is started later"). Under
`systemd.sysusers` or `services.userborn`, two units appear:
`agenix-install-secrets` (`before = [ "systemd-sysusers.service" ]`, "So user
passwords can be encrypted") and `agenix-chown` (`after` sysusers and the
install unit, since ownership can only be set once users exist). Otherwise,
activation scripts `agenixNewGeneration` (after `specialfs`) and
`agenixInstall`.

`age.identityPaths` defaults to the rsa and ed25519 entries of
`config.services.openssh.hostKeys`, with an assertion: "age.identityPaths must
be set, for example by enabling openssh."

### Key rotation

From
[`doc/rekeying.md`](https://github.com/ryantm/agenix/blob/main/doc/rekeying.md):

> If you change the public keys in `agenix-rules.nix`, you should rekey your
> secrets: `agenix --rekey`
>
> To rekey a secret, you have to be able to decrypt it. Because of randomness in
> `age`'s encryption algorithms, the files always change when rekeyed, even if
> the identities do not.

Two consequences. You need decrypt access to every secret you rekey, so adding a
host is a workstation operation, not a CI one. And because ciphertext always
changes, a rekey produces a diff touching every secret file, which makes review
noisy. There is no single-secret rekey; that is
[open issue #143](https://github.com/ryantm/agenix/issues/143).

### Threat model

agenix documents its own weaknesses better than any of the others, and the
contents are load-bearing. From
[`doc/threat-model-warnings.md`](https://github.com/ryantm/agenix/blob/main/doc/threat-model-warnings.md):

> This project has not been audited by a security professional.
>
> People unfamiliar with `age` might be surprised that secrets are not
> authenticated. This means that every attacker that has write access to the
> secret files can modify secrets because public keys are exposed. [...]
> reviewing configuration changes is easier than reviewing random secrets (for
> example, 4096-bit rsa keys). This would be solved by having a message
> authentication code (MAC) like other implementations like GPG or
> [sops](https://github.com/Mic92/sops-nix) have, however this was left out for
> simplicity in `age`.
>
> Encrypt only secrets that you can rotate if their contents are exposed in the
> future. As of June 2024, [age is not post-quantum safe]. Someone who can
> collect encrypted files today may retain them and decrypt them if future
> advances make that possible.

The missing-MAC point is a genuine differentiator in sops-nix's favour for any
repository with more than one writer, or any repository where a compromised CI
token could push.

### Multi-host and home-manager

Multi-host scoping is a `publicKeys` list per path in `agenix-rules.nix`, with
host pubkeys obtained via `ssh-keyscan` and user pubkeys optionally pulled from
`https://github.com/<user>.keys`. There is no grouping primitive beyond what Nix
expressions give you, which in practice means people build their own `let`
bindings of host lists.

The home-manager module is
[`modules/age-home.nix`](https://github.com/ryantm/agenix/blob/main/modules/age-home.nix).
It has the same user-not-root limitation as sops-nix's, and I verified the
sharpest consequence directly against source: **the file contains zero mount
commands** (no `mount -t`, no `tmpfs`, no `ramfs`, no `hdiutil`), unlike the
NixOS module. Secrets therefore land in `$XDG_RUNTIME_DIR/agenix` (and
`$(getconf DARWIN_USER_TEMP_DIR)` on macOS) with whatever properties that
directory already has. Delivery is `systemd.user.services.agenix`
(`Description = "agenix activation"`, `Type = "oneshot"`,
`Install.WantedBy = [ "default.target" ]`) on Linux and a launchd agent on
Darwin.

### CI/CD

Weaker than sops-nix by construction. The default identity is the machine's SSH
host key, so a CI runner holds no usable identity unless you deliberately add a
CI age key or SSH key as a recipient in the rules file and inject the private
half. Builds themselves need no secrets, since the `.age` files are in the
store. Note also [issue #223](https://github.com/ryantm/agenix/issues/223),
"Agenix cli error in pure evaluation mode", which bites automation that runs the
CLI under `--pure-eval`.

---

## agenix-rekey (oddlama/agenix-rekey)

Repo metrics, 2026-11-04: 448 stars, 59 forks, 35 open issues, created
2023-01-30, `pushed_at` 2026-10-09T12:08:01Z, not archived, MIT. No releases;
consumers track the flake.

### What it actually is

Not a fork. From the
[README](https://github.com/oddlama/agenix-rekey/blob/main/README.md): "This is
an extension for [agenix](https://github.com/ryantm/agenix) which allows you to
get rid of maintaining a `secrets.nix` file by automatically re-encrypting
secrets where needed. [...] This extension is a flakes-only project and can be
used alongside regular use of agenix."

The model inverts agenix's: you encrypt each secret **once, to a master
identity** (YubiKey, FIDO2, TPM, or a plain age identity), and agenix-rekey
re-encrypts it to each host's public key. Secrets declare `rekeyFile` instead of
`file`. The README's claimed benefits include "Host-key inference. No need to
manually keep track of which key is needed for which host - no `secrets.nix`"
and, notably, "a leaked host-key doesn't allow an attacker to decrypt older
checked-in secrets, in case your repo is public."

### The impurity problem, stated by the author

This is the most useful paragraph in the whole corpus for understanding why
rekeying is a separate step everywhere:

> The central problem is that rekeying secrets on-the-fly while building your
> system is fundamentally impossible, since it is an impure operation. It will
> always require an external input in the form of your master password or has to
> communicate with a YubiKey.
>
> The second problem is that building your system requires the rekeyed secrets
> to be available in the nix-store, which we want to achieve without requiring
> you to track them in git.

### Storage modes, and why this is the CI decision point

`age.rekey.storageMode` is an `enum ["derivation" "local"]`, default `"local"`.
The README's own tradeoff text:

**`local`** (current default): "All rekeyed secrets will be saved to a local
folder in your flake when running `agenix rekey`."

- Pro: "System building stays pure, no need for sandbox shenanigans. -> System
  can be built without access to the (yubi)key."
- Con: "If your repository is public and one of your hosts is compromised, an
  attacker may decrypt any secret that was ever encrypted for that host. This
  includes secrets that are in the git history."

**`derivation`** (former default): "All rekeyed secrets for each host will be
collected in a derivation which copies them to the nix store when it is built
using `agenix rekey`." The derivation "is made to always fail when the build is
invoked transitively by the build process, which always means rekeying is
necessary", and `agenix rekey` builds the same derivation with sandbox access to
a predictable `/tmp` path.

- Pro: "The entire process is stateless and rekeyed secrets are never committed
  to your repository."
- Con: "You cannot easily build your host from a CI/any host that hasn't access
  to your (yubi)key except by manually uploading the derivations to the CI after
  rekeying."

`derivation` mode also requires
`nix.settings.extra-sandbox-paths = ["/tmp/agenix-rekey.${config.users.users.youruser.uid}"]`,
with a warning worth reproducing verbatim: "(DO NOT add your user to
trusted-users instead, this would basically grant them root access!)". For
remote deploys, "Any tool that builds locally and uses `nix copy` [...] will
work automatically [...] Only when you strictly build on your remotes, you might
have to copy those secrets manually."

So: **if CI must build your hosts, use `local`.** The README says so plainly,
and the storage-mode docs frame `derivation` as for people who "only build on
one host and don't care about remote building / CI".

### Key rotation: the best story of the four

Master-key rotation is a first-class verb. The subcommand list:

> - `agenix generate`: Generates any secrets that don't exist yet and have a
>   generator set.
> - `agenix view`: View secrets using fzf.
> - `agenix edit`: Create/edit secrets using `$EDITOR`. Can encrypt existing
>   files.
> - `agenix rekey`: Rekeys secrets for hosts that require them.
> - `agenix update-masterkeys`: Rekeys secrets for a new set of masterkeys.

Host rotation is also better than agenix's: changing `hostPubkey` triggers a
rekey prompt rather than a rules-file edit, and the README notes rekeying is
lazy and cached ("Rekeying only occurs when necessary"), with the build failing
to tell you when it is needed. New-host bootstrap is handled by rekeying to a
dummy pubkey so a host with no key yet still builds.

### Generation

Unique among the four tools: `age.generators` plus
`age.secrets.<name>.generator` let you declare how a secret is produced, with
dependencies between generators, tags, and `settings` parameterisation, so
secrets can be bootstrapped rather than hand-created.
`age.rekey.generatedSecretsDir` sets a default location. For a fleet where most
secrets are machine-generated keypairs and passwords rather than human-chosen
values, this removes most of the manual work the other tools leave in place.

### Key-handling guardrails

- `hostPubkey`: "Make sure to NEVER use a private key here, as it will end up in
  the public nix store!"
- `masterIdentities`, recommended forms: "Use a split-identity ending in `.pub`,
  where the private part is not contained (a yubikey identity)"; "Use an
  absolute path to your key outside the nix store"; or "encrypt your age
  identity and use the extension `.age`". Followed by: "Be careful when using
  paths here, as they will be copied to the nix store."
- `extraEncryptionPubkeys` for a break-glass backup recipient that is not tried
  during normal decryption.
- `AGENIX_REKEY_PRIMARY_IDENTITY` for multi-operator setups "where each person
  only has access to their own personal Yubikey".
- `agePlugins` defaults to `[rekeyHostPkgs.age-plugin-yubikey]`.

### Home-manager: not supported

I checked the repository tree: the only Nix modules are
`modules/agenix-rekey.nix` and `flake-module.nix`. There is no home-manager
module. The two open issues from users discovering this are
[#94](https://github.com/oddlama/agenix-rekey/issues/94) ("Home-Manager module
doesn't work/can't get agenix-rekey to decrypt my secrets that's declared on
Home-Manager") and [#68](https://github.com/oddlama/agenix-rekey/issues/68)
("Secrets referenced by home-manager are being deleted immediately after
rekeying").

---

## ragenix (yaxitech/ragenix)

Repo metrics, 2026-11-04: 483 stars, 27 forks, 21 open issues, created
2021-05-07, Apache-2.0, not archived. `main` HEAD **2025-10-30**; see the
`pushed_at` caveat at the top of this document.

### Mechanism: deliberately identical to agenix

From the [README](https://github.com/yaxitech/ragenix/blob/main/README.md):

> `ragenix` provides age-encrypted secrets for NixOS systems which live in the
> Nix store and are decrypted on system activation.
>
> `ragenix` is a drop-in replacement for [@ryantm]'s `agenix` written in Rust.
> It aims at being fully compatible with its flake while offering more robust
> command line parsing, additional validation logic, plugin support, shell
> completions, and solid tests.

Critically, for understanding what ragenix does and does not change: "The flake
also exposes a NixOS and Darwin module which is **passed through from the
`agenix` flake**." So runtime behaviour, paths, mount type, activation ordering,
and the home-manager module are all agenix's. Only the CLI is reimplemented. The
security properties of ragenix are exactly agenix's security properties.

What it adds over the shell script: argument parsing, validation, `--schema`
(prints the JSON schema rules must conform to), plugin support, shell
completions for bash/zsh/fish. It is flakes-only: "**As opposed to `agenix`,
`ragenix` only strives for supporting Nix Flakes**."

### Rotation, multi-host, CI

All inherited. `ragenix --rekey` is agenix's rekey; `--rules` defaults to
`./secrets.nix` with a `RULES` env override. Contributor note on CI: "The CI
invokes `nix flake check`. Some of the checks invoke `nix` itself. To allow
those tests to run `nix`, you have to enable the `recursive-nix` feature."

### Observable drift from agenix

The compatibility promise is the whole value proposition, so drift is the thing
to check. Evidence that it has drifted:

- **Rules filename.** ragenix still defaults to `./secrets.nix`. Upstream agenix
  now prefers `agenix-rules.nix` and searches parent directories for that name
  only.
- [#158](https://github.com/yaxitech/ragenix/issues/158): "Support decrypt flag
  the same way Agenix does" (missing CLI parity).
- [#159](https://github.com/yaxitech/ragenix/issues/159): "ragenix does not
  build on Darwin 25.05".
- [#148](https://github.com/yaxitech/ragenix/issues/148): "Documentation thats
  not just pointing to agenix?"
- [#134](https://github.com/yaxitech/ragenix/issues/134): "Change decryption
  directory: `/run/agenix` -> `/run/ragenix`?"

A drop-in replacement whose upstream is shipping releases while it sits at a
year-old commit is a liability rather than an upgrade.

---

## The rest of the ecosystem

### NixOS Wiki: Comparison of secret managing schemes

[The wiki page](https://wiki.nixos.org/wiki/Comparison_of_secret_managing_schemes)
is the only broad inventory, and is useful for discovering tools: beyond
sops-nix, agenix and ragenix it lists **nix-secrets**, **krops**, **secrix**
("`age` encryption minimizing decrypted duration"), **vaultix** ("`age` Rust
crate encryption"), NixOps `deployment.keys`, and terraform-nixos.

**It is also easy to misread, and I nearly did.** Its columns are: Scheme,
Pre-build, Build time, In the store, System activation, Runtime, Encryption
technology, Usable project, Templating support, Additional notes. The column
named **"Build time" means "what occurs during the build process"**, not "when
decryption happens". A first pass at this page produced a table asserting
"decryption timing: build time" for both agenix and sops-nix, which directly
contradicts both projects' own documentation and would invert the security
conclusion. Treat the wiki as a tool index, and take timing claims from each
project's source.

### clan.lol `vars`

Clan is a full fleet-management framework; `vars` is its
secrets-and-generated-files layer
([docs](https://clan.lol/docs/26.05/guides/vars/vars-concepts)). A _generator_
is a script producing files, which may take user prompts and depend on other
generators. The type split is the interesting part: secret vars are reachable
only via `.path` and "are deployed to `/run/secrets/` on the target machine (or
`/run/secrets-for-users/` when `neededFor = 'users'`)", while public vars are
reachable via `.value` and live in the Nix store. Storage is pluggable: "sops
(default), password-store, age, custom".

Those runtime paths and the `neededFor`/`neededForUsers` naming are sops-nix's
exactly, which strongly suggests sops-nix underneath for the NixOS case, though
clan's own docs do not say so (see open questions). Conceptually, clan `vars` is
to sops-nix roughly what agenix-rekey is to agenix: it adds **generation** and
removes per-host bookkeeping.

Repo metrics for the GitHub mirror `clan-lol/clan-core`, 2026-11-04: 181 stars,
`pushed_at` 2026-10-26. The star count understates adoption, because the
description identifies it as a "read-only mirror of
https://git.clan.lol/clan/clan-core"; the canonical repo is self-hosted and its
stars are not counted on GitHub.

### cachix `secretspec`

Repo metrics, 2026-11-04: 1634 stars, 34 open issues, `pushed_at`
2026-11-04T12:16:52Z, not archived. Tagline: "A declarative interface for every
secret provider", with "One contract. Development, CI, and production."
([secretspec.dev](https://secretspec.dev/)). It is a _resolver_ rather than an
encrypted-file format, covering roughly 35 providers including keyring, KeePass,
1Password, Vault/OpenBao, Infisical, Bitwarden, AWS/Azure/GCP, and also age,
SOPS, Kubernetes and systemd credentials. Its appeal is that the same secret
declaration resolves from a developer's keyring locally and from Vault in
production.

Its [`nixos.md`](https://github.com/cachix/secretspec/blob/main/nixos.md)
describes NixOS integration as "a resolver and reconciliation layer" whose
invariant is keeping "plaintext out of the Nix store and persistent files", with
delivery through "ordinary `LoadCredentialEncrypted=` entries" and persistent
encrypted credential blobs under `/etc/secretspec/credentials/sets/`, in either
a "target pull" or "controller push" security model. **Read that document as a
phased design spec rather than shipped behaviour**: it describes a phased
approach and references "Controller push (0.18+)", so parts are forward-looking.
This is the most interesting thing on the horizon, and not yet a thing to bet a
fleet on.

### vals

[`helmfile/vals`](https://github.com/helmfile/vals) substitutes `ref+<backend>`
and `secretref+<backend>` URIs in YAML/JSON with values fetched from Vault, AWS
SSM / Secrets Manager / S3 / KMS, GCP, and Azure, so manifests in git contain
references rather than secrets. It is packaged in nixpkgs. It is **not a NixOS
activation mechanism**: substitution happens at render time, in a pipeline,
outside Nix's evaluation and outside activation. It is the right tool for
Helm/Kubernetes flows and the wrong frame for NixOS system secrets, where the
whole point is that the host decrypts for itself.

### systemd credentials

`systemd-creds` plus `LoadCredential=` / `LoadCredentialEncrypted=` is the
direction the NixOS module ecosystem is drifting, because credentials are passed
to a service through a private per-unit directory rather than a filesystem path
anyone can stat. It does not replace sops/age for git-committed secrets:
`systemd-creds` encryption is symmetric and host-bound (TPM or host key), so the
encryption has to happen _on the machine_, which is the opposite of what a
committed-secrets workflow needs. The two compose, with sops/age owning
at-rest-in-git and systemd owning delivery-to-process.

Current state: neither sops-nix nor agenix has a native `LoadCredential` option;
you wire it manually to the decrypted path.
[agenix #123](https://github.com/ryantm/agenix/issues/123) ("Integration with
systemd-creds") is open. `josh/systemd-age-creds` exists as a bridge, and
nixpkgs has per-service `LoadCredential` requests such as
[nixpkgs #377889](https://github.com/NixOS/nixpkgs/issues/377889). sops-nix's
`useSystemdActivation` is a step toward this world but is about _installation_
ordering, not credential delivery.

---

## Community sentiment

### A disclosure about sources

The task asked for Reddit r/NixOS threads. **I could not obtain any.**
`reddit.com` is not accessible to this agent's fetcher (direct fetch returns
"unable to fetch from www.reddit.com", and domain-restricted search returns an
explicit error naming `reddit.com` as inaccessible to the user agent). A
`discourse.nixos.org` search fetch returned a placeholder page rather than
results. Rather than paraphrase Reddit consensus I cannot read, the sentiment
below is drawn from **GitHub issue trackers**, which are primary, verifiable,
and were queried live via the GitHub search API on 2026-11-04. Where I cite an
issue I am reporting its title and comment count, which is evidence of what
people actually hit, not a claim about majority opinion.

### sops-nix: the recurring complaints are ordering and home-manager

Most-commented open issues (`sort:comments-desc`, 2026-11-04):

| #                                                   | Title                                                           | Comments |
| --------------------------------------------------- | --------------------------------------------------------------- | -------- |
| [65](https://github.com/Mic92/sops-nix/issues/65)   | Some SSH RSA keys cannot be used for decryption                 | 32       |
| [149](https://github.com/Mic92/sops-nix/issues/149) | Secrets not showing up in `/run/secrets` after reboot           | 31       |
| [24](https://github.com/Mic92/sops-nix/issues/24)   | This runs too early in the boot process if included in an image | 24       |
| [478](https://github.com/Mic92/sops-nix/issues/478) | Sops nix as a home-manager module does not create a symlink     | 23       |
| [198](https://github.com/Mic92/sops-nix/issues/198) | Access secret from systemd service with `DynamicUser=true`      | 20       |
| [377](https://github.com/Mic92/sops-nix/issues/377) | Age key that lives on a Yubikey via age-plugin-yubikey          | 20       |
| [273](https://github.com/Mic92/sops-nix/issues/273) | How to best support templating?                                 | 13       |
| [356](https://github.com/Mic92/sops-nix/issues/356) | sops-nix always requests yubikey with home-manager module       | 11       |
| [744](https://github.com/Mic92/sops-nix/issues/744) | remove ssh-to-pgp rsa instructions                              | 11       |
| [225](https://github.com/Mic92/sops-nix/issues/225) | how to enforce a dependency on sops-nix for systemd services?   | 6        |
| [217](https://github.com/Mic92/sops-nix/issues/217) | Have a systemd target to indicate successful decryption?        | 6        |

The clustering is informative. #149, #24, #225 and #217 are all the same
underlying complaint: _activation ordering is hard to reason about and there is
no target to depend on_. That is precisely the gap `useSystemdActivation` is
closing. #478 and #356 say the home-manager module is the rough edge, which
matches the README's own admission that it is a feature subset. #65 and #744
mark `ssh-to-pgp` / RSA as a legacy trap; the modern path is age plus
`ssh-to-age`. Notably, **key rotation friction is not among the top issues** for
sops-nix, which is mild evidence that `sops updatekeys` being per-file is
annoying rather than blocking.

### agenix: missing features, and home-manager again

| #                                                  | Title                                                                   | Comments |
| -------------------------------------------------- | ----------------------------------------------------------------------- | -------- |
| [305](https://github.com/ryantm/agenix/issues/305) | Home manager module conflict                                            | 23       |
| [23](https://github.com/ryantm/agenix/issues/23)   | RFC: agenix-rs tool                                                     | 17       |
| [84](https://github.com/ryantm/agenix/issues/84)   | Feature: restart systemd service when relevant secret updated           | 16       |
| [4](https://github.com/ryantm/agenix/issues/4)     | Support for ssh-keys with passphrases                                   | 13       |
| [223](https://github.com/ryantm/agenix/issues/223) | Agenix cli error in pure evaluation mode                                | 13       |
| [143](https://github.com/ryantm/agenix/issues/143) | rekey a single secret?                                                  | 6        |
| [123](https://github.com/ryantm/agenix/issues/123) | Integration with systemd-creds                                          | 5        |
| [193](https://github.com/ryantm/agenix/issues/193) | Agenix symlinks incorrectly when using agenix secrets as initrd secrets | 5        |
| [219](https://github.com/ryantm/agenix/issues/219) | agenix doesn't work properly when using impermanence                    | 5        |
| [332](https://github.com/ryantm/agenix/issues/332) | Yubikey not detected during boot using age-plugin-yubikey               | 5        |
| [89](https://github.com/ryantm/agenix/issues/89)   | Don't decrypt if secrets haven't changed from previous activation       | 4        |

The standouts, read against sops-nix's feature set: **#84 is open**, meaning
agenix still has no equivalent of `sops.secrets.<n>.restartUnits`, so rotating a
secret does not restart its consumer. **#143 is open**, confirming rekey is
all-or-nothing. **#4 is open since early in the project**, so
passphrase-protected SSH keys remain unsupported, which forces an unattended
key. #23 ("RFC: agenix-rs tool", 17 comments) is the conversation that ragenix
and `agenix-cli` grew out of, and the fact that agenix remains a shell script
tells you how that resolved. #219 (impermanence) and #193 (initrd) are the two
deployment shapes where people most often get hurt.

### agenix-rekey: provisioning and home-manager

| #                                                         | Title                                                                           | Comments |
| --------------------------------------------------------- | ------------------------------------------------------------------------------- | -------- |
| [104](https://github.com/oddlama/agenix-rekey/issues/104) | agenix generator for ssh-ed25519 is failing                                     | 8        |
| [70](https://github.com/oddlama/agenix-rekey/issues/70)   | agenix rekey does not support submodules?                                       | 7        |
| [106](https://github.com/oddlama/agenix-rekey/issues/106) | How to use agenix-rekey to set env var for nushell through home-manager?        | 7        |
| [37](https://github.com/oddlama/agenix-rekey/issues/37)   | Rekeying zero secrets leads to error                                            | 6        |
| [68](https://github.com/oddlama/agenix-rekey/issues/68)   | Secrets referenced by home-manager are being deleted immediately after rekeying | 5        |
| [97](https://github.com/oddlama/agenix-rekey/issues/97)   | running `agenix edit` causes 500+ package build                                 | 4        |
| [60](https://github.com/oddlama/agenix-rekey/issues/60)   | Discussion: improve user journeys when provisioning new hosts                   | 3        |

#60 is the honest one: new-host provisioning is still acknowledged as rough even
in the tool whose entire premise is removing new-host friction. #97 (a secret
edit triggering a 500-package build) is the practical cost of putting the whole
workflow behind flake evaluation. #68 and #106 confirm that home-manager usage
is attempted and unsupported.

### Secondary sources

For completeness, the better-quality secondary write-ups surfaced during search,
flagged as secondary and not used for any mechanism claim above: Michael
Stapelberg,
["Secret Management on NixOS with sops-nix (2025)"](https://michael.stapelberg.ch/posts/2025-08-24-secret-management-with-sops-nix/),
and LGUG2Z,
["Handling Secrets in NixOS: An Overview"](https://lgug2z.com/articles/handling-secrets-in-nixos-an-overview/).

---

## Recommendation

### Default answer for a multi-host fleet with CI: sops-nix

Not because it is more popular, but because the feature gaps between it and
agenix are exactly the ones a fleet runs into:

1. **Secret rotation actually restarts consumers.** `restartUnits` /
   `reloadUnits` ship in the module. agenix's equivalent request, #84, is open
   after years. Without this, rotating a credential silently leaves the old one
   live in a running process.
2. **Secrets are authenticated.** sops carries a MAC; agenix's own threat-model
   doc says age does not, and that "every attacker that has write access to the
   secret files can modify secrets". With multiple committers or a CI token that
   can push, that is a real integrity gap.
3. **Templates** solve the very common case of a service wanting a config file
   with a secret inside it, rather than a path.
4. **Ordering has an answer.** `useSystemdActivation` auto-engages under
   `systemd-sysusers` / `userborn`, and gives you a unit to order against.
5. **CI decryption is a solved pattern**, via a dedicated CI age identity in
   `creation_rules` and `SOPS_AGE_KEY_FILE`, with no host key in the runner.
6. **`pathNotInStore` on `age.keyFile`** is a type-level refusal of the mistake
   that matters most.
7. **Activity.** Commits within two days of this writing.

Accept in exchange: `sops updatekeys` is per-file and manual, so adding a host
is a scripted loop over your secret files; and there are no semver releases, so
pin a flake revision deliberately.

### Single-host homelab, or anyone who wants to read the whole tool: agenix

CC0, deliberately tiny ("Very little code, so it should be easy for you to
audit"), no GPG, and it reuses the SSH host keys you already have, so setup is
one `ssh-keyscan` away. Its runtime hygiene is good: tmpfs with `noswap` on
kernels at least 6.4, ramfs below that, correct `umask` plus atomic rename, and
the same sysusers-aware ordering split as sops-nix. The threat-model document is
the most honest in the ecosystem.

The costs are acceptable at one host and compound at ten: no restart-on-change,
no single-secret rekey (so every recipient change rewrites every ciphertext and
produces a noisy diff), no MAC, no passphrase on the identity, and a rules file
you maintain by hand.

### Many hosts, hardware token, mostly machine-generated secrets: agenix-rekey on top of agenix

This is the strongest rotation story of the four and the only one with secret
_generation_. `agenix update-masterkeys` makes master-key rotation a command
rather than a project. Host rotation needs a `hostPubkey` change and a prompted
rekey, with no rules file to edit. Generators mean per-host keypairs and
passwords are declared rather than hand-made, which is where fleet toil actually
lives.

Two conditions. Set `storageMode = "local"` if CI builds your hosts: the README
is unambiguous that `derivation` mode cannot easily build from a CI without the
YubiKey. And if you choose `local`, keep the repository **private**, because the
documented consequence of a public repo plus one compromised host is that an
attacker can decrypt every secret ever encrypted for that host, including from
git history. If the repo must be public, use `derivation` mode and build
locally.

### home-manager only, no NixOS root: sops-nix's HM module, with eyes open

Use `homeManagerModules.sops`, and understand that the threat model is weaker in
a way no tool can fix, because **activation runs as the user**. There is no
ramfs (sops-nix says so; agenix's HM module I verified contains no mount call at
all), no ownership enforcement, and the identity must be passphrase-free so a
service can use it unattended. Secrets land under `$XDG_RUNTIME_DIR`, and any
consumer must declare `Unit.After = [ "sops-nix.service" ]` itself, since
delivery is a systemd user service rather than an activation step. Expect the
rough edges in #478 and #356.

agenix's HM module is comparably limited (plus #305, "Home manager module
conflict"). agenix-rekey is not an option here: it has no HM module.

If a user-session secret guards anything that matters, the better answer is to
move it to the system module and grant the user read access via group, rather
than hardening the HM path.

### Already running clan.lol: use `vars`

It gives you sops-equivalent at-rest encryption plus generators plus fleet
management in one model, and the public/secret type split (`.value` versus
`.path`) is a genuinely good design: it makes "accidentally used a secret at
evaluation time" a type error rather than a review catch.

### Do not adopt ragenix in 2026

`main` has not moved in about a year, there are zero releases, and the
compatibility promise that is its entire value proposition is visibly drifting
(rules filename, missing `--decrypt` parity in #158, Darwin 25.05 build failure
in #159). The deeper point: because ragenix passes agenix's NixOS and Darwin
modules straight through, **you get Rust only in the CLI**, which is the part
you invoke occasionally and by hand. The runtime, where correctness matters, is
agenix either way. Existing users are not in danger, but new adoption buys a
stale dependency for no runtime benefit.

### Watch, do not yet adopt: secretspec and systemd credentials

`secretspec` is the most interesting development here, and its NixOS document
targets the right invariant (plaintext never in the store or in persistent
files) with the right delivery mechanism (`LoadCredentialEncrypted=`). But that
document reads as a phased design with parts still ahead of the implementation.
Revisit in two quarters. The broader move toward `LoadCredential` is real and
worth designing _toward_, since it beats a filesystem path for process
isolation, but today it composes behind sops/age rather than replacing them.

### Note for xi

xi's `xi-sources-credentials` spec covers git transport credentials (ssh, https,
oauth), which is a different problem from at-rest secret encryption. The useful
conclusion for xi is a negative one: **xi should not grow its own
encrypted-secret-at-rest format.** The ecosystem has converged on
sops/age-encrypted files in git, decrypted at activation into
`/run/secrets`-style paths. xi's leverage is to _consume_ those paths (treat a
secret as a path provided by the host, resolved after the relevant unit has run)
and to order its daemon after the host's secret-installation unit, which is
`sops-install-secrets.service` or `agenix-install-secrets.service` under
sysusers, or an activation script otherwise. That ordering dependency is worth
writing into the spec explicitly, because the top sops-nix issues (#149, #225,
#217) are all consumers that got it wrong.

---

## Open questions

1. **Does clan `vars` use sops-nix underneath, or reimplement it?** Clan's
   runtime paths (`/run/secrets/`, `/run/secrets-for-users/`) and its
   `neededFor = "users"` naming match sops-nix exactly, which is strong
   circumstantial evidence, but clan's own docs never state it and a GitHub code
   search over `clan-lol/clan-core` returned nothing (the GitHub copy is a
   read-only mirror and may not be indexed). Resolving this needs the canonical
   repo at `git.clan.lol`. It matters: if clan wraps sops-nix, clan users
   inherit sops-nix's ordering semantics and its issue tracker.

2. **Is `secretspec`'s NixOS integration shipped?** Its `nixos.md` describes a
   phased design and references "Controller push (0.18+)", so some of what it
   documents is forward-looking. The gap between documented and implemented is
   unresolved from the repo alone.

3. **Community sentiment is under-sampled, by constraint not by choice.** Reddit
   is inaccessible to this agent's fetcher and a Discourse search fetch returned
   a placeholder, so the sentiment section rests on GitHub issue trackers only.
   Issue trackers over-represent breakage and under-represent satisfaction, so
   read the sentiment section as "what people hit", not "what people prefer".
   Anyone wanting true sentiment should read r/NixOS and Discourse manually.

4. **sops-nix has no versioned releases.** The only tag is `assets` from 2021.
   Whether rolling `master` is a risk or simply the project's norm is a judgment
   call, not a fact the sources settle. It does mean there is no changelog to
   diff before bumping.

5. **Post-quantum exposure is acknowledged by exactly one of the four.** agenix
   documents age's harvest-now-decrypt-later risk and tells you to "Encrypt only
   secrets that you can rotate". sops-nix's docs do not discuss it, despite
   age-encrypted sops files having the same property. This is unresolved for
   both, and arguably understated by the quieter one.

6. **No authoritative comparison exists, and the closest thing misleads.** The
   NixOS Wiki page is the only broad inventory and its "Build time" column is
   routinely read as "decryption timing", which inverts the security conclusion.
   An upstream fix or a clearer column name would prevent a recurring
   misunderstanding.
