#!/bin/sh
# fetch-or-build.sh — herdr [[build]] step for herdr-file-viewer.
#
# Fast path: download the prebuilt binary that matches THIS source's declared version + platform
# from the GitHub release, verify its SHA-256, and install it at target/release/herdr-file-viewer.
# The match is by VERSION, not by exact commit: a checkout that is AHEAD of the matching release
# (e.g. main has merged work that isn't tagged yet) still uses that released binary, so landing a
# PR no longer forces new users to compile while a release is pending. Integrity is unchanged — the
# binary is still SHA-256 verified — and a version with no published release still 404s to source.
# Fallback: on ANY miss (no asset for this version, network/download error, checksum mismatch,
# unmapped platform, no curl/wget) print a clear notice and build from source with cargo — identical
# to the pre-prebuilt behavior, so installing never gets harder than before.
#
# Paths and the release base URL are overridable via env (FV_REPO_ROOT / FV_CARGO_TOML / FV_OUT /
# FV_BASE_URL) so the logic is exercised by a hermetic test with stubbed uname/curl/cargo/git.
set -u

repo="smarzban/herdr-file-viewer"

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root="${FV_REPO_ROOT:-$script_dir/..}"
cargo_toml="${FV_CARGO_TOML:-$repo_root/Cargo.toml}"
out="${FV_OUT:-$repo_root/target/release/herdr-file-viewer}"
base_url="${FV_BASE_URL:-https://github.com/$repo/releases/download}"

have() { command -v "$1" >/dev/null 2>&1; }

# Build from source — the original, unconditional behavior. Source ~/.cargo/env so cargo is found
# even when herdr was launched without ~/.cargo/bin on PATH (e.g. a GUI / login-less launch); the
# `[ -f ]` guard means a missing env file can't abort the build. Clear message if cargo is absent.
build_from_source() {
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  if ! have cargo; then
    echo "herdr-file-viewer needs Rust 1.96+ to build, but cargo was not found. Install Rust from https://rustup.rs then re-run: herdr plugin install $repo" >&2
    exit 1
  fi
  exec cargo build --release
}

fallback() {
  echo "herdr-file-viewer: $1 — building from source instead." >&2
  [ -n "${tmpdir:-}" ] && rm -rf "$tmpdir"
  build_from_source
}

download() { # download <url> <dest>
  if have curl; then
    curl -fsSL -o "$2" "$1"
  elif have wget; then
    wget -q -O "$2" "$1"
  else
    return 127
  fi
}

sha256_of() { # prints the hex digest of file $1
  if have sha256sum; then
    sha256sum "$1" | awk '{print $1}'
  elif have shasum; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    return 127
  fi
}

# --- resolve the target triple from the platform ------------------------------------------
os=$(uname -s 2>/dev/null || echo unknown)
arch=$(uname -m 2>/dev/null || echo unknown)
triple=""
case "$os" in
  Darwin)
    case "$arch" in
      arm64|aarch64) triple="aarch64-apple-darwin" ;;
      x86_64|amd64)  triple="x86_64-apple-darwin" ;;
    esac
    ;;
  Linux)
    case "$arch" in
      x86_64|amd64)  triple="x86_64-unknown-linux-musl" ;;
    esac
    ;;
esac
[ -n "$triple" ] || fallback "no prebuilt binary for $os/$arch"

# --- read the version this source declares ------------------------------------------------
version=$(grep -E '^version *= *"' "$cargo_toml" 2>/dev/null | head -n 1 | sed -E 's/^version *= *"([^"]+)".*/\1/')
[ -n "$version" ] || fallback "could not read version from $cargo_toml"

asset="herdr-file-viewer-$triple"

tmpdir=$(mktemp -d 2>/dev/null) || fallback "could not create a temp dir"
trap 'rm -rf "$tmpdir"' EXIT

# --- version-only match (no commit-exactness gate) -----------------------------------------
# The prebuilt is used whenever a release exists for the version this source DECLARES — even when
# the checkout is ahead of that release's commit (e.g. main has merged work that isn't tagged yet).
# This decouples "merge a PR" from "cut a release": new installs keep getting the last released,
# SHA-256-verified binary instead of being forced to compile. A version with no matching release
# still falls back to source, because the asset download further below 404s — so we can never
# silently install a binary whose version differs from this source.
#
# For transparency only (never a failure): if this is a git work tree and we can read both HEAD and
# the release's published COMMIT marker, note when the checkout is ahead — the binary is the released
# v$version while the working tree may carry newer, unreleased source. `git rev-parse HEAD` is a safe
# local ref read; the COMMIT fetch is best-effort and its absence is not an error.
ahead_note=""
if have git && git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  head_rev=$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || echo nohead)
  if download "$base_url/v$version/COMMIT" "$tmpdir/COMMIT" 2>/dev/null; then
    release_commit=$(tr -d '[:space:]' < "$tmpdir/COMMIT" 2>/dev/null)
    if [ -n "$release_commit" ] && [ "$head_rev" != "$release_commit" ]; then
      ahead_note=" Note: this checkout ($head_rev) is ahead of the v$version release commit ($release_commit), so newer unreleased source is not in this binary."
    fi
  fi
fi

bin_url="$base_url/v$version/$asset"
sums_url="$base_url/v$version/SHA256SUMS"
tmpbin="$tmpdir/$asset"
tmpsums="$tmpdir/SHA256SUMS"

download "$bin_url" "$tmpbin"   || fallback "prebuilt binary not available for v$version ($asset)"
download "$sums_url" "$tmpsums" || fallback "checksums not available for v$version"

# Expected hash = the SHA256SUMS line for our asset filename. The separator before the name is
# `  ` (two spaces, coreutils text mode) here, but `sha256sum`'s binary mode emits ` *name`; accept
# either marker (`[ *]`) so a binary-mode line (e.g. from Git-for-Windows on the release runner)
# still verifies rather than silently forcing a source build.
expected=$(grep -E "^[0-9a-f]{64} [ *]$asset\$" "$tmpsums" 2>/dev/null | awk '{print $1}' | head -n 1)
[ -n "$expected" ] || fallback "no checksum listed for $asset"

actual=$(sha256_of "$tmpbin") || fallback "no sha-256 tool (sha256sum/shasum) available"
if [ "$actual" != "$expected" ]; then
  fallback "checksum mismatch for $asset (expected $expected, got $actual)"
fi

# Verified — make it executable and move it into place.
chmod +x "$tmpbin"
mkdir -p "$(dirname "$out")"
mv -f "$tmpbin" "$out" || fallback "could not install the verified binary to $out"
echo "herdr-file-viewer: installed prebuilt v$version ($triple), verified SHA-256.$ahead_note"
exit 0
