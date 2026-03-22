#!/bin/sh
# install.sh — Install Sonda pre-built binaries.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh
#
# Environment variables:
#   SONDA_VERSION      — version to install (e.g. "v0.1.0"). Defaults to latest release.
#   SONDA_INSTALL_DIR  — installation directory. Defaults to /usr/local/bin.
#
# Supported platforms:
#   linux   x86_64, aarch64
#   darwin  x86_64, aarch64 (arm64)

set -e

REPO="davidban77/sonda"
GITHUB_API="https://api.github.com"
GITHUB_RELEASES="https://github.com/${REPO}/releases/download"

main() {
    need_cmd uname

    # Detect OS
    os="$(uname -s)"
    case "$os" in
        Linux)  os="linux" ;;
        Darwin) os="darwin" ;;
        *)
            err "unsupported operating system: $os"
            ;;
    esac

    # Detect architecture
    arch="$(uname -m)"
    case "$arch" in
        x86_64|amd64)   arch="x86_64" ;;
        aarch64|arm64)  arch="aarch64" ;;
        *)
            err "unsupported architecture: $arch"
            ;;
    esac

    # Map to Rust target triple
    case "${os}-${arch}" in
        linux-x86_64)   target="x86_64-unknown-linux-musl" ;;
        linux-aarch64)  target="aarch64-unknown-linux-musl" ;;
        darwin-x86_64)  target="x86_64-apple-darwin" ;;
        darwin-aarch64) target="aarch64-apple-darwin" ;;
        *)
            err "unsupported platform: ${os}-${arch}"
            ;;
    esac

    # Resolve version
    version="${SONDA_VERSION:-}"
    if [ -z "$version" ]; then
        say "fetching latest release version..."
        need_cmd_either curl wget
        version="$(get_latest_version)"
        if [ -z "$version" ]; then
            err "could not determine latest release version"
        fi
    fi

    say "installing sonda ${version} for ${target}"

    # Resolve install directory
    install_dir="${SONDA_INSTALL_DIR:-/usr/local/bin}"

    # Build download URLs
    tarball_name="sonda-${version}-${target}.tar.gz"
    tarball_url="${GITHUB_RELEASES}/${version}/${tarball_name}"
    checksums_url="${GITHUB_RELEASES}/${version}/SHA256SUMS"

    # Create a temporary directory for downloads
    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "$tmp_dir"' EXIT

    # Download tarball and checksums
    say "downloading ${tarball_url}"
    download "$tarball_url" "${tmp_dir}/${tarball_name}"

    say "downloading checksums"
    download "$checksums_url" "${tmp_dir}/SHA256SUMS"

    # Verify checksum
    say "verifying checksum..."
    verify_checksum "${tmp_dir}" "${tarball_name}"
    say "checksum verified"

    # Extract binaries
    say "extracting to ${install_dir}"
    ensure_dir "$install_dir"
    tar xzf "${tmp_dir}/${tarball_name}" -C "$tmp_dir"

    # Install binaries
    install_binary "${tmp_dir}/sonda" "${install_dir}/sonda"
    install_binary "${tmp_dir}/sonda-server" "${install_dir}/sonda-server"

    say ""
    say "sonda ${version} installed successfully"
    say "  sonda:        ${install_dir}/sonda"
    say "  sonda-server: ${install_dir}/sonda-server"
    say ""
    say "run 'sonda --help' to get started"
}

say() {
    printf 'sonda-install: %s\n' "$1"
}

err() {
    say "ERROR: $1" >&2
    exit 1
}

need_cmd() {
    if ! command -v "$1" > /dev/null 2>&1; then
        err "required command not found: $1"
    fi
}

need_cmd_either() {
    for cmd in "$@"; do
        if command -v "$cmd" > /dev/null 2>&1; then
            return 0
        fi
    done
    err "one of the following commands is required: $*"
}

download() {
    url="$1"
    dest="$2"
    if command -v curl > /dev/null 2>&1; then
        if ! curl -fsSL -o "$dest" "$url"; then
            err "failed to download: $url"
        fi
    elif command -v wget > /dev/null 2>&1; then
        if ! wget -q -O "$dest" "$url"; then
            err "failed to download: $url"
        fi
    else
        err "either curl or wget is required"
    fi
}

get_latest_version() {
    url="${GITHUB_API}/repos/${REPO}/releases/latest"
    if command -v curl > /dev/null 2>&1; then
        response="$(curl -fsSL "$url")" || err "failed to fetch latest release info"
    elif command -v wget > /dev/null 2>&1; then
        response="$(wget -q -O - "$url")" || err "failed to fetch latest release info"
    else
        err "either curl or wget is required"
    fi

    # Extract tag_name from JSON response without jq
    # Matches "tag_name": "v0.1.0" and extracts v0.1.0
    version="$(printf '%s' "$response" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
    if [ -z "$version" ]; then
        err "could not parse version from GitHub API response"
    fi
    printf '%s' "$version"
}

verify_checksum() {
    dir="$1"
    filename="$2"
    expected="$(grep "$filename" "${dir}/SHA256SUMS" | awk '{print $1}')"

    if [ -z "$expected" ]; then
        err "checksum not found for ${filename} in SHA256SUMS"
    fi

    if command -v sha256sum > /dev/null 2>&1; then
        actual="$(cd "$dir" && sha256sum "$filename" | awk '{print $1}')"
    elif command -v shasum > /dev/null 2>&1; then
        actual="$(cd "$dir" && shasum -a 256 "$filename" | awk '{print $1}')"
    else
        err "sha256sum or shasum is required for checksum verification"
    fi

    if [ "$expected" != "$actual" ]; then
        err "checksum mismatch for ${filename}: expected ${expected}, got ${actual}"
    fi
}

ensure_dir() {
    dir="$1"
    if [ -d "$dir" ]; then
        if [ -w "$dir" ]; then
            return 0
        fi
    fi

    # Try creating or writing to the directory with sudo if needed
    if mkdir -p "$dir" 2>/dev/null; then
        return 0
    fi

    say "elevated permissions required for ${dir}"
    if command -v sudo > /dev/null 2>&1; then
        sudo mkdir -p "$dir"
    else
        err "cannot create directory ${dir} — run as root or set SONDA_INSTALL_DIR"
    fi
}

install_binary() {
    src="$1"
    dest="$2"

    if [ ! -f "$src" ]; then
        err "binary not found in archive: $src"
    fi

    if cp "$src" "$dest" 2>/dev/null; then
        chmod +x "$dest"
    elif command -v sudo > /dev/null 2>&1; then
        sudo cp "$src" "$dest"
        sudo chmod +x "$dest"
    else
        err "cannot install to ${dest} — run as root or set SONDA_INSTALL_DIR"
    fi
}

main "$@"
