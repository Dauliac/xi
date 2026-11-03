#!/bin/sh
# xi bootstrap stub.
#
# Stage one of a two-stage bootstrap: detect the host, download the xi
# binary, verify it against a hash templated in at release time, and
# exec it with the caller's arguments. Everything after that - preflight,
# Nix install, clone, cache prestage, switch, receipt - lives in the
# binary, so a bare machine and a managed one take the same command:
#
#   curl -sSf -L https://install.xi.dev \
#     | sh -s -- system switch https://git.example/dotfiles
#
# Spec: openspec/changes/xi-sources-bootstrap/specs/xi-sources-bootstrap/spec.md
#
# POSIX sh on purpose: this runs before anything is installed, on hosts
# whose /bin/sh is dash, ash, or the FreeBSD shell.

set -eu

# --------------------------------------------------------------------------
# Release-time template
# --------------------------------------------------------------------------

# Where the release assets live. XI_BOOTSTRAP_URL_OVERRIDE lets an
# operator point the stub at their own mirror, a file:// URL, or an
# absolute local path (which is also how the test suite drives it).
XI_BASE_URL="${XI_BOOTSTRAP_URL_OVERRIDE:-https://github.com/Dauliac/xi/releases/latest/download}"

# One hash per published target, templated at release time. The literal
# placeholders are what ships in-tree; the release pipeline rewrites
# them. An unreplaced placeholder is refused rather than skipped, so an
# unsigned stub can never silently run an unverified binary.
XI_SHA256_LINUX_X86_64='@SHA256_LINUX_X86_64@'
XI_SHA256_LINUX_AARCH64='@SHA256_LINUX_AARCH64@'
XI_SHA256_DARWIN_X86_64='@SHA256_DARWIN_X86_64@'
XI_SHA256_DARWIN_AARCH64='@SHA256_DARWIN_AARCH64@'

# --------------------------------------------------------------------------
# Plumbing
# --------------------------------------------------------------------------

XI_TMPDIR=''

cleanup() {
  if [ -n "$XI_TMPDIR" ] && [ -d "$XI_TMPDIR" ]; then
    rm -rf "$XI_TMPDIR"
  fi
}
trap cleanup EXIT HUP INT TERM

say() {
  printf 'xi-bootstrap: %s\n' "$1" >&2
}

die() {
  printf 'xi-bootstrap: error: %s\n' "$1" >&2
  exit 1
}

have() {
  command -v "$1" >/dev/null 2>&1
}

# --------------------------------------------------------------------------
# OS / arch detection
# --------------------------------------------------------------------------

# Snap-packaged curl cannot read or write outside $HOME, which breaks
# every download into a temp dir. Refusing it and falling through to
# wget is better than the confusing permission error it produces.
curl_is_snap() {
  _curl=$(command -v curl 2>/dev/null) || return 1
  case "$_curl" in
  /snap/* | */snap/bin/*) return 0 ;;
  esac
  if [ -L "$_curl" ] && have readlink; then
    _resolved=$(readlink -f "$_curl" 2>/dev/null || printf '%s' "$_curl")
    case "$_resolved" in
    /snap/*) return 0 ;;
    esac
  fi
  return 1
}

detect_os() {
  _uname_s=$(uname -s)
  case "$_uname_s" in
  Linux) printf 'linux' ;;
  Darwin) printf 'darwin' ;;
  *) die "unsupported operating system '$_uname_s'" ;;
  esac
}

# Rosetta disambiguation: under Rosetta 2, `uname -m` reports x86_64 on
# an Apple Silicon host. hw.optional.arm64 is the authoritative answer,
# and picking the wrong one here installs an emulated binary that then
# installs an emulated Nix.
detect_arch() {
  _os=$1
  _uname_m=$(uname -m)
  case "$_uname_m" in
  x86_64 | amd64)
    if [ "$_os" = darwin ] && have sysctl &&
      [ "$(sysctl -n hw.optional.arm64 2>/dev/null || printf 0)" = 1 ]; then
      printf 'aarch64'
    else
      printf 'x86_64'
    fi
    ;;
  aarch64 | arm64) printf 'aarch64' ;;
  *) die "unsupported architecture '$_uname_m'" ;;
  esac
}

expected_sha256() {
  case "$1" in
  linux-x86_64) printf '%s' "$XI_SHA256_LINUX_X86_64" ;;
  linux-aarch64) printf '%s' "$XI_SHA256_LINUX_AARCH64" ;;
  darwin-x86_64) printf '%s' "$XI_SHA256_DARWIN_X86_64" ;;
  darwin-aarch64) printf '%s' "$XI_SHA256_DARWIN_AARCH64" ;;
  *) die "no published binary for '$1'" ;;
  esac
}

# --------------------------------------------------------------------------
# Download
# --------------------------------------------------------------------------

# curl first, then wget, then fetch (FreeBSD). HTTPS is pinned on every
# one of them: this script's whole job is fetching an executable, so a
# downgrade to plaintext is not a fallback worth having.
download() {
  _url=$1
  _dest=$2

  case "$_url" in
  file://*)
    _path=$(printf '%s' "$_url" | sed -e 's|^file://||')
    [ -f "$_path" ] || die "no such file: $_path"
    cp "$_path" "$_dest"
    return 0
    ;;
  /*)
    [ -f "$_url" ] || die "no such file: $_url"
    cp "$_url" "$_dest"
    return 0
    ;;
  esac

  if have curl && ! curl_is_snap; then
    curl --proto '=https' --tlsv1.2 --fail --silent --show-error \
      --location --output "$_dest" "$_url" ||
      die "download failed: $_url"
    return 0
  fi

  if have curl; then
    # The literal name of the variable, not its value: a snap-confined
    # curl cannot write outside the operator's home at all. Spelled with
    # a directive rather than `\$HOME` in double quotes because shfmt
    # rewrites that back to this form, and the two would fight.
    # shellcheck disable=SC2016
    say 'ignoring snap-packaged curl (it cannot write outside $HOME)'
  fi

  if have wget; then
    wget --https-only --secure-protocol=TLSv1_2 --quiet \
      --output-document="$_dest" "$_url" ||
      die "download failed: $_url"
    return 0
  fi

  if have fetch; then
    # FreeBSD's fetch verifies peers by default; -q keeps it quiet.
    fetch -q -o "$_dest" "$_url" ||
      die "download failed: $_url"
    return 0
  fi

  die "no usable downloader found; install curl, wget, or fetch"
}

sha256_of() {
  if have sha256sum; then
    sha256sum "$1" | cut -d' ' -f1
  elif have shasum; then
    shasum -a 256 "$1" | cut -d' ' -f1
  elif have sha256; then
    sha256 -q "$1"
  elif have openssl; then
    openssl dgst -sha256 "$1" | sed -e 's/^.*= //'
  else
    die "no sha256 tool found; install coreutils, perl's shasum, or openssl"
  fi
}

# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------

os=$(detect_os)
arch=$(detect_arch "$os")
target="$os-$arch"

# XI_BOOTSTRAP_SHA256 pins the hash from outside the file, for operators
# mirroring a build of their own and for the test suite. It is the only
# way past the templated hash, and it still has to match.
expected="${XI_BOOTSTRAP_SHA256:-$(expected_sha256 "$target")}"
case "$expected" in
'@SHA256_'*)
  die "this stub was not templated at release time: no sha256 for \
$target. Set XI_BOOTSTRAP_SHA256 if you are running it from a checkout."
  ;;
esac

XI_TMPDIR=$(mktemp -d 2>/dev/null || mktemp -d -t xi-bootstrap)
binary="$XI_TMPDIR/xi"

url="$XI_BASE_URL/xi-$target"
say "downloading $url"
download "$url" "$binary"

actual=$(sha256_of "$binary")
if [ "$actual" != "$expected" ]; then
  # Abort before the binary is ever made executable, let alone exec'd.
  rm -f "$binary"
  die "sha256 mismatch for $url
  expected: $expected
    actual: $actual
Refusing to run an unverified binary."
fi
say "sha256 verified"

chmod +x "$binary"

# Reattach the terminal. Under `curl | sh` the script's stdin is the
# pipe, so the planner's confirmation prompt would read EOF and the
# whole flow would abort or, worse, proceed unasked. Everything the
# child prompts for needs a tty; XI_BOOTSTRAP_NO_CONFIRM is the way out
# when there is none.
if [ ! -t 0 ]; then
  # Probing with `[ -r /dev/tty ]` is not enough: the device node is
  # readable in containers and CI runners that have no controlling
  # terminal, and the open then fails mid-redirect. Opening it in a
  # subshell is the only honest test.
  if (: </dev/tty) 2>/dev/null; then
    exec 0</dev/tty
  elif [ -z "${XI_BOOTSTRAP_NO_CONFIRM:-}" ]; then
    die "stdin is not a terminal and /dev/tty is unavailable.
Set XI_BOOTSTRAP_NO_CONFIRM=1 to run without prompts."
  fi
fi

# The temp dir leaks on the success path: there is nothing left to run
# the cleanup trap after exec, and the alternative (fork, wait, delete)
# would put a shell between the operator's terminal and xi's prompts.
# Upstream nix-installer and rustup make the same trade.
trap - EXIT HUP INT TERM
exec "$binary" "$@"
