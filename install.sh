#!/usr/bin/env bash
# install.sh — one-shot installer for the `sumo` CLI (sigmakee-rs).
#
# 1. Downloads the latest `sigmakee-v*` GitHub release for your platform,
# 2. Installs it under $SIGMA_HOME/bin
# 3. Wires SIGMA_HOME + PATH into your shell environment
# 4. Generates a starter config.xml.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/ontologyportal/sigma-rs/main/install.sh | bash
#
# Env overrides:
#   SIGMA_HOME    Install root (default: $HOME/.sigmakee)
#   SUMO_VERSION  Specific release tag to install, e.g. sigmakee-v2.0.0
#                 (default: the latest sigmakee-v* release)
#
# macOS (arm64/x64) and Linux (x64/arm64, glibc) only

set -euo pipefail

REPO_OWNER="ontologyportal"
REPO_NAME="sigma-rs"
TAG_PREFIX="sigmakee-v"

SIGMA_HOME="${SIGMA_HOME:-$HOME/.sigmakee}"
export SIGMA_HOME
BIN_DIR="$SIGMA_HOME/bin"
ENV_FILE="$SIGMA_HOME/env"

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# Detect platform
detect_label() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64) echo "darwin-arm64" ;;
        x86_64)        echo "darwin-x64" ;;
        *) die "Unsupported macOS architecture: $arch" ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64)        echo "linux-x64-gnu" ;;
        aarch64|arm64) echo "linux-arm64-gnu" ;;
        *) die "Unsupported Linux architecture: $arch" ;;
      esac
      ;;
    *)
      die "Unsupported OS: $os. Windows: grab win32-x64 by hand from https://github.com/$REPO_OWNER/$REPO_NAME/releases, or build from source."
      ;;
  esac
}

# Resolve the release tag
# GitHub's release list is newest-first, and this repo publishes releases
# under more than one tag prefix (e.g. `sumo-lsp-v*` for a different crate),
# so the global "latest release" is NOT necessarily a sumo release — filter
# to `sigmakee-v*` and take the first match.
resolve_tag() {
  if [ -n "${SUMO_VERSION:-}" ]; then
    printf '%s\n' "$SUMO_VERSION"
    return
  fi
  local releases tag
  releases="$(curl -fsSL "https://api.github.com/repos/$REPO_OWNER/$REPO_NAME/releases")" \
    || die "Could not reach the GitHub API"
  tag="$(printf '%s' "$releases" \
    | grep -o "\"tag_name\": *\"${TAG_PREFIX}[^\"]*\"" \
    | head -n1 \
    | sed -E 's/.*"([^"]+)"$/\1/')" || true
  [ -n "$tag" ] || die "Could not find a ${TAG_PREFIX}* release — check https://github.com/$REPO_OWNER/$REPO_NAME/releases"
  printf '%s\n' "$tag"
}

# Download, verify, install the binary
install_binary() {
  local label="$1" tag="$2"
  local ver="${tag#"$TAG_PREFIX"}"
  local staging="sumo-${ver}-${label}"
  local archive="${staging}.tar.gz"
  local base_url="https://github.com/$REPO_OWNER/$REPO_NAME/releases/download/$tag"

  local tmp
  tmp="$(mktemp -d)"

  info "Downloading $archive ($tag)…"
  curl -fsSL -o "$tmp/$archive" "$base_url/$archive" \
    || die "Download failed: $base_url/$archive"
  curl -fsSL -o "$tmp/$archive.sha256" "$base_url/$archive.sha256" \
    || die "Download failed: $base_url/$archive.sha256"

  info "Verifying checksum…"
  (
    cd "$tmp"
    if command -v shasum >/dev/null 2>&1; then
      shasum -a 256 -c "$archive.sha256"
    elif command -v sha256sum >/dev/null 2>&1; then
      sha256sum -c "$archive.sha256"
    else
      warn "no shasum/sha256sum found — skipping checksum verification"
    fi
  ) || die "Checksum verification failed — the download may be corrupt"

  tar -xzf "$tmp/$archive" -C "$tmp"
  [ -f "$tmp/$staging/sumo" ] || die "Archive didn't contain the expected sumo binary"

  mkdir -p "$BIN_DIR"
  cp "$tmp/$staging/sumo" "$BIN_DIR/sumo"
  chmod +x "$BIN_DIR/sumo"
  info "Installed sumo $ver → $BIN_DIR/sumo"

  rm -rf "$tmp"
}

# Wire SIGMA_HOME + PATH into the shell environment
write_env_file() {
  mkdir -p "$SIGMA_HOME"
  cat > "$ENV_FILE" <<EOF
# Added by the sigmakee-rs installer (install.sh). Re-running the installer
# regenerates this file; it's safe to \`source\` more than once.
export SIGMA_HOME="$SIGMA_HOME"
case ":\${PATH}:" in
  *":$BIN_DIR:"*) ;;
  *) export PATH="$BIN_DIR:\$PATH" ;;
esac
EOF
  info "Wrote $ENV_FILE"
}

# Idempotently point one shell rc file at $ENV_FILE, guarded by a marker
# comment so re-running the installer never duplicates the line. Creates the
# rc file if it doesn't exist yet (e.g. a fresh macOS account has no
# ~/.zshrc even though zsh is the default shell).
add_source_line() {
  local rc="$1"
  touch "$rc" 2>/dev/null || return 0
  grep -qF "$ENV_FILE" "$rc" 2>/dev/null && return 0
  {
    echo ''
    echo '# Added by the sigmakee-rs installer'
    echo ". \"$ENV_FILE\""
  } >> "$rc"
  info "Updated $rc"
}

update_shell_rc() {
  add_source_line "$HOME/.bashrc"
  add_source_line "$HOME/.zshrc"
  add_source_line "$HOME/.profile"
}

# Generate a starter config.xml
generate_config() {
  info "Generating \$SIGMA_HOME/KBs/config.xml…"
  # A flag forces `sumo config`'s write mode deterministically — bare
  # `sumo config` launches the interactive TUI in a real terminal (wrong for
  # an unattended installer) or just prints a dump when piped (never writes
  # anything). `--base-dir` seeds the one setting that actually matters at
  # install time; every other field is written with its built-in default.
  "$BIN_DIR/sumo" config --base-dir "$SIGMA_HOME"

  # Older releases (before `sumo config` gained a write mode) accept the
  # flag but only ever print a dump — exit 0 either way, so the only real
  # signal is whether the file actually landed.
  if [ ! -f "$SIGMA_HOME/KBs/config.xml" ]; then
    warn "config.xml was not created — this sumo build may predate 'sumo config' write support."
    warn "Create it by hand once a newer release is available: sumo config --base-dir \"$SIGMA_HOME\""
  fi
}

# Declare a starter "SUMO" <kb> (Merge.kif + the usual base ontology files)
# in config.xml. The files themselves aren't downloaded by this installer —
# `sumo --git <repo> --branch <name> load` (or a manual clone into kbDir) is
# what actually fetches them — so `--declare` skips the usual existence
# check `sumo config --kb NAME -f ...` would otherwise enforce. Idempotent:
# re-adding an already-declared constituent is a no-op (`add_constituents_to_kb`
# dedups), and it never touches an already-set sumokbname.
declare_sumo_kb() {
  [ -f "$SIGMA_HOME/KBs/config.xml" ] || return 0
  "$BIN_DIR/sumo" config --kb SUMO --declare \
    -f english_format.kif -f domainEnglishFormat.kif -f Merge.kif -f Mid-level-ontology.kif \
    >/dev/null || warn "could not declare the starter SUMO <kb> in config.xml (this sumo build may predate --declare support)"
}

main() {
  local label tag
  label="$(detect_label)"
  tag="$(resolve_tag)"

  install_binary "$label" "$tag"
  write_env_file
  update_shell_rc
  generate_config
  declare_sumo_kb

  echo
  info "Done. Installed: $("$BIN_DIR/sumo" --version)"
  info "Restart your shell, or run:  source \"$ENV_FILE\""
  info "config.xml declares a SUMO KB but hasn't fetched its files yet — see"
  info "README.md's Quick start for loading it (e.g. \`sumo --git <repo> --branch <name> load\`)."
}

main "$@"
