#!/bin/sh
# Installs borg, borg-mesh and borg-web from a GitHub release: picks the tarball for this OS and CPU,
# checks it against its .sha256, copies the binaries to ~/.local/bin (or BORG_INSTALL_DIR), then offers
# `borg setup`. Linux x86_64/aarch64 (glibc) and macOS aarch64; elsewhere: cargo install borg-ai.
#
# usage: scripts/install.sh [--version vX.Y.Z] [--dir DIR] [--no-setup]
#        curl -fsSL https://raw.githubusercontent.com/debe/borg_ai/main/scripts/install.sh | sh
# env:   BORG_VERSION (default: the latest release), BORG_INSTALL_DIR (default: ~/.local/bin),
#        BORG_DOWNLOAD_URL (a mirror of https://github.com/debe/borg_ai/releases/download)
#
# Everything runs from main on the last line, so a download cut off mid-transfer runs nothing.
set -eu

REPO=debe/borg_ai

die() { echo "install.sh: $*" >&2; exit 1; }

# fetch URL FILE (FILE "-" writes to stdout)
fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        die "needs curl or wget"
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        die "needs sha256sum or shasum to check the download"
    fi
}

target() {
    case $(uname -m) in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) arch=$(uname -m) ;;
    esac
    # A shell under Rosetta reports x86_64 on Apple silicon; the arm64 build runs there natively.
    if [ "$(uname -s)" = Darwin ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
        arch=aarch64
    fi
    # The Linux builds link glibc; on musl (Alpine) they do not start.
    if [ "$(uname -s)" = Linux ] && ldd --version 2>&1 | grep -qi musl; then
        die "no release build for musl Linux; build it with: cargo install borg-ai --locked"
    fi
    case $(uname -s)-$arch in
        Linux-x86_64 | Linux-aarch64) echo "$arch-unknown-linux-gnu" ;;
        Darwin-aarch64) echo aarch64-apple-darwin ;;
        *) die "no release build for $(uname -s) $arch; build it with: cargo install borg-ai --locked" ;;
    esac
}

latest() {
    fetch "https://api.github.com/repos/$REPO/releases/latest" - |
        sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1
}

main() {
    version=${BORG_VERSION:-}
    dir=${BORG_INSTALL_DIR:-$HOME/.local/bin}
    setup=yes
    while [ $# -gt 0 ]; do
        case $1 in
            --version) [ $# -ge 2 ] || die "--version needs a value"; version=$2; shift 2 ;;
            --dir) [ $# -ge 2 ] || die "--dir needs a value"; dir=$2; shift 2 ;;
            --no-setup) setup=no; shift ;;
            -h | --help) echo "usage: install.sh [--version vX.Y.Z] [--dir DIR] [--no-setup]"; exit 0 ;;
            *) die "unknown argument: $1 (see --help)" ;;
        esac
    done

    t=$(target)
    [ -n "$version" ] || version=$(latest)
    [ -n "$version" ] || die "could not find the latest release of $REPO; pass --version vX.Y.Z"
    case $version in v*) ;; *) version=v$version ;; esac

    name="borg-$version-$t"
    url="${BORG_DOWNLOAD_URL:-https://github.com/$REPO/releases/download}/$version/$name.tar.gz"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' INT TERM

    echo "downloading $url"
    fetch "$url" "$tmp/$name.tar.gz" || die "download failed: $url"
    fetch "$url.sha256" "$tmp/$name.tar.gz.sha256" || die "download failed: $url.sha256"
    want=$(cut -d' ' -f1 <"$tmp/$name.tar.gz.sha256")
    got=$(sha256 "$tmp/$name.tar.gz")
    if [ -z "$want" ] || [ "$want" != "$got" ]; then
        die "sha256 mismatch for $name.tar.gz: expected $want, got $got"
    fi
    echo "sha256 ok: $got"

    tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
    mkdir -p "$dir"
    for b in borg borg-mesh borg-web; do
        cp "$tmp/$name/$b" "$dir/$b.tmp.$$"
        chmod 755 "$dir/$b.tmp.$$"
        mv -f "$dir/$b.tmp.$$" "$dir/$b"
    done
    rm -rf "$tmp"
    echo "installed borg, borg-mesh and borg-web $version to $dir"

    case :$PATH: in
        *":$dir:"*) ;;
        *) echo "note: $dir is not on PATH; add it, e.g. export PATH=\"$dir:\$PATH\"" ;;
    esac

    # Setup asks before it writes anything; it needs a terminal, which `curl ... | sh` leaves on /dev/tty.
    if [ "$setup" = yes ] && [ -t 1 ] && ( : </dev/tty ) 2>/dev/null; then
        printf 'run borg setup now? [Y/n] '
        read -r answer </dev/tty || answer=n
        case $answer in
            '' | y | Y | yes) exec "$dir/borg" setup </dev/tty ;;
        esac
    fi
    echo "next: borg setup"
}

main "$@"
