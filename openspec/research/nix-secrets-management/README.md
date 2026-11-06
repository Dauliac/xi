# Nix secrets management - research index

This directory holds research on the Nix / NixOS **secrets manager** ecosystem
as of 2025-2026: the tools that keep plaintext out of the world-readable Nix
store and deliver it to services at activation time. This is adjacent to, but
distinct from, [[xi-sources-credentials]], which covers git _transport_ auth
(ssh / https / oauth) for the pull-GitOps daemon. The research exists because
xi's own secret and credential handling should interoperate with whatever the
ecosystem has converged on rather than inventing a parallel format, and because
xi's daemon has a hard ordering dependency on the host's secret-installation
unit.

## Research notes (primary sources, cite-rich)

- [overview.md](./overview.md) - sops-nix, agenix, agenix-rekey, ragenix,
  clan.lol `vars`, cachix `secretspec`, `vals`, systemd credentials. Mechanism
  and decryption-timing audit per tool (read from module source, not blog
  posts), rotation and multi-host models, CI/CD patterns, live GitHub
  maintenance metrics, GitHub-issue-derived sentiment, and a per-use-case
  recommendation.

## Headline findings

- All four age/sops tools decrypt at **activation**, never at build. The
  encrypted file in the Nix store is the design, not a leak. The NixOS Wiki
  comparison table is easy to misread as saying the opposite.
- **sops-nix** for a multi-host fleet with CI: it has `restartUnits`, templates,
  a MAC on secrets, and `useSystemdActivation` for ordering. **agenix** for a
  single auditable host. **agenix-rekey** when you have a hardware token and
  want secret _generation_ plus `update-masterkeys` rotation. **ragenix is
  dormant** (`main` last moved 2025-10-30, zero releases) and passes agenix's
  modules straight through, so it buys Rust only in the CLI.
- home-manager is the weak spot everywhere, because activation runs as the user:
  no ramfs, no chown, passphrase-free identity required. agenix-rekey has no HM
  module at all.
- Recommendation for xi: do not grow an encrypted-at-rest format. Consume
  host-provided secret paths and order the daemon after
  `sops-install-secrets.service` / `agenix-install-secrets.service`.

## Open questions carried forward

1. Does clan.lol `vars` wrap sops-nix or reimplement it? Its runtime paths and
   `neededFor` naming match sops-nix exactly, but its docs never say, and the
   GitHub copy is an unindexed mirror of a self-hosted repo.
2. Is `secretspec`'s NixOS `LoadCredentialEncrypted=` integration shipped or
   still design? Its `nixos.md` reads as a phased spec and cites "0.18+".
3. Community sentiment is GitHub-issue-only: reddit.com is inaccessible to the
   research agent's fetcher and a Discourse search fetch returned a placeholder,
   so the sentiment section reflects what people _hit_, not what they _prefer_.
