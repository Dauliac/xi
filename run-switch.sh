#!/usr/bin/env bash
set -euo pipefail
exec /home/juliendauliac/ghq/github.com/Dauliac/xi/target/debug/xi \
  system switch git@github.com:Dauliac/dotfiles.git --branch master
