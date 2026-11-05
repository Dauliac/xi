# How to Iterate on Your Config Without a Push Round-Trip

Use this while actively editing your config: point xi at your local checkout as
the remote itself, so a switch picks up every commit without a push to GitHub or
GitLab in between.

## Point the remote at a local path instead of a URL

`remote` accepts an absolute local path exactly like it accepts a `git@`,
`ssh://`, or `https://` URL, it is still just a git transport underneath:

```nix
programs.xi.sources = {
  enable = true;
  remote = "/home/you/ghq/github.com/you/dotfiles";
  workingDir = "/home/you/ghq/github.com/you/dotfiles";
};
```

If a gitops daemon is polling this scope, it fetches from the local path on its
normal interval, same as it would from GitHub. If you are switching by hand
instead, commit and switch:

```sh
git -C ~/ghq/github.com/you/dotfiles commit -am "wip"
xi home switch
```

No push needed, the switch reads straight from local git history.

Keep `remote` and `workingDir` pointing at different things if you actually want
the three-way reconciliation (`xi <scope> sources
status|diff|sync`) to mean
anything: if both name the same directory, the sources dir only ever mirrors
your own commits back at you, and the ahead/behind/diverged matrix never has an
independent remote to compare against. For pure local-iteration speed with no
reconciliation story, that collapse is harmless, you are trading it away on
purpose.

## If you are iterating on xi itself, not just your config

Override xi's own flake input for one invocation, without touching `flake.lock`:

```sh
xi system switch -- --override-input xi \
  "git+file:///home/you/ghq/github.com/you/xi?ref=my-branch"
```

Everything after `--` passes straight to the underlying `nix build`. This
rebuilds xi from your branch and uses it for this switch only; your committed
`flake.lock` still points wherever it normally does.

## Preview without activating

```sh
xi home switch --dry
```

Builds and reports what would change, without switching. At system scope this
still needs to run unprivileged and still needs `/var/lib/xi` to already exist,
the dry-run flag skips activation, not the sources lock.
