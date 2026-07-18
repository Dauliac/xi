# How to Search Packages, Options, PRs, and Issues

## Search packages

```sh
xi search hello
```

This queries search.nixos.org and returns package names, versions, and
descriptions.

Shorthand `xi search <query>` defaults to package search. Change the default
with `XI_DEFAULT_SEARCH=options`.

## Search NixOS and Home Manager options

```sh
xi search options services.nginx
```

Filter by scope:

```sh
xi search options --scope nixpkgs services.nginx
xi search options --scope home-manager programs.git
xi search options --scope all services
```

## Search offline with SPAM databases

If you have a [SPAM](https://github.com/feel-co/spam) database:

```sh
xi search offline --db /path/to/nixpkgs.db hello
```

Multiple databases:

```sh
xi search offline --db db1.json --db db2.json hello
```

Or set `XI_OFFLINE_DB=/path/to/db1.json:/path/to/db2.json`.

## Search Nixpkgs pull requests

```sh
xi search prs "firefox update"
```

Fetch a specific PR by number:

```sh
xi search prs 12345
xi search prs "#12345"
```

Merged PRs show which Nixpkgs branches they have reached.

Limit the time window:

```sh
xi search prs --days 7 "package"
```

## Search Nixpkgs issues

```sh
xi search issues --days 30 "segfault"
```

## Common flags

Output as JSON (all modes):

```sh
xi search --json hello
```

Limit results:

```sh
xi search --limit 10 hello
```

Select channel:

```sh
xi search --channel nixos-24.11 hello
```

Show supported platforms:

```sh
xi search --platforms hello
```

## GitHub authentication

`xi search prs` and `xi search issues` use GitHub's API. Authentication sources
(in priority order):

1. `GH_TOKEN` environment variable
2. `XI_GITHUB_TOKEN_FILE` pointing to a file containing the token
3. `$XDG_STATE_HOME/xi/github-token` (default token file location)
4. Interactive prompt (saves the token to the default file)
