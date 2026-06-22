#!/usr/bin/env bash
# Install the viewer's OPTIONAL external renderers — glow (markdown), delta (diffs),
# bat (syntax) — using whatever package manager this machine has.
#
# These are runtime, install-time dependencies, NOT Cargo deps. The viewer works without
# them (it falls back to plain text + a notice), so this script is a convenience, never a
# requirement. It is idempotent: already-installed renderers are skipped. It never uses sudo
# implicitly — system package managers are invoked with sudo only where they need it, and you
# can read exactly what runs below.
set -u

have() { command -v "$1" >/dev/null 2>&1; }

# --- detect a package manager (first match wins) ------------------------------------------
PM=""
if have brew; then PM="brew"
elif have apt-get; then PM="apt"
elif have dnf; then PM="dnf"
elif have pacman; then PM="pacman"
fi

# Map a renderer to its package name for the detected manager. Empty = "not packaged here".
pkg_name() {
  local tool="$1"
  case "$PM:$tool" in
    brew:glow|apt:glow|dnf:glow|pacman:glow) echo "glow" ;;
    brew:delta|apt:delta|dnf:delta|pacman:delta) echo "git-delta" ;;
    brew:bat|apt:bat|dnf:bat|pacman:bat) echo "bat" ;;
    *) echo "" ;;
  esac
}

pm_install() {
  local pkg="$1"
  case "$PM" in
    brew)   brew install "$pkg" ;;
    apt)    sudo apt-get update -qq && sudo apt-get install -y "$pkg" ;;
    dnf)    sudo dnf install -y "$pkg" ;;
    pacman) sudo pacman -S --noconfirm "$pkg" ;;
    *) return 1 ;;
  esac
}

# cargo fallback for the two renderers that ship as crates (glow is Go — no cargo install).
cargo_pkg() {
  case "$1" in
    delta) echo "git-delta" ;;
    bat)   echo "bat" ;;
    *)     echo "" ;;
  esac
}

install_one() {
  local tool="$1" bin="$2"   # bin = the command name to test for (delta/bat/glow)
  if have "$bin"; then
    echo "✓ $tool already installed ($(command -v "$bin"))"
    return 0
  fi
  local pkg; pkg="$(pkg_name "$tool")"
  if [ -n "$PM" ] && [ -n "$pkg" ] && pm_install "$pkg"; then
    # On Debian/Ubuntu the bat package installs its binary as `batcat`, not `bat` (the name
    # the viewer looks for). Bridge it via a symlink in ~/.local/bin.
    if [ "$tool" = "bat" ] && ! have bat && have batcat; then
      mkdir -p "$HOME/.local/bin"
      ln -sf "$(command -v batcat)" "$HOME/.local/bin/bat"
      if have bat; then
        echo "✓ installed bat via $PM (bridged 'batcat' → ~/.local/bin/bat)"
      else
        echo "✓ installed bat via $PM as 'batcat', symlinked to ~/.local/bin/bat — add ~/.local/bin to PATH to use it"
      fi
      return 0
    fi
    # Confirm the binary is actually on PATH before claiming success (a package can install
    # under a different name, as bat does above).
    if have "$bin"; then
      echo "✓ installed $tool via $PM ($pkg)"
      return 0
    fi
    echo "… '$pkg' installed via $PM but '$bin' is not on PATH; trying alternatives"
  fi
  # Fall back to cargo where possible.
  local cp; cp="$(cargo_pkg "$tool")"
  if [ -n "$cp" ] && have cargo; then
    echo "… trying cargo install $cp"
    if cargo install "$cp" && have "$bin"; then echo "✓ installed $tool via cargo ($cp)"; return 0; fi
  fi
  echo "✗ could not put '$bin' on PATH for $tool — install it manually:"
  case "$tool" in
    glow)  echo "    https://github.com/charmbracelet/glow#installation" ;;
    delta) echo "    https://github.com/dandavison/delta#installation  (or: cargo install git-delta)" ;;
    bat)   echo "    https://github.com/sharkdp/bat#installation  (or: cargo install bat)" ;;
  esac
  return 1
}

echo "herdr-file-viewer — optional renderers"
if [ -n "$PM" ]; then echo "package manager: $PM"; else echo "no supported package manager found (brew/apt/dnf/pacman) — will try cargo where possible"; fi
echo

rc=0
install_one glow  glow  || rc=1
install_one delta delta || rc=1
install_one bat   bat   || rc=1

echo
if [ "$rc" -eq 0 ]; then
  echo "All renderers available — you'll get rendered markdown, syntax-highlighted diffs, and code."
else
  echo "Some renderers are missing; the viewer still works (plain text + a notice for those views)."
fi
exit 0
