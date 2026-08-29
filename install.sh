#!/usr/bin/env bash
# chef one-line installer (Git Bash, MSYS2, WSL, or any Unix shell).
#   curl -fsSL https://raw.githubusercontent.com/x87/chef/master/install.sh | bash
#
# Mirrors install.ps1: same release asset, same checksum sidecar, same
# chef.old rollback. chef itself is Windows-only; this script fetches the
# Windows binary and puts it in <CHEF_HOME>/bin.
set -euo pipefail

REPO="x87/chef"
CHEF_HOME="${CHEF_HOME:-${LOCALAPPDATA:-$HOME/AppData/Local}/Chef}"
BIN_DIR="$CHEF_HOME/bin"
ASSET="chef-x86_64-pc-windows-msvc.zip"

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }

echo "resolving latest release of $REPO..."
release="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest")"
url="$(printf '%s' "$release" | sed -n "s/.*\"browser_download_url\": *\"\([^\"]*$ASSET\)\".*/\1/p" | head -n1)"
if [ -z "$url" ]; then
    echo "error: could not find asset $ASSET in the latest release" >&2
    exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "downloading $ASSET..."
zip="$tmp/$ASSET"
curl -fSL "$url" -o "$zip"

# Verify SHA-256 against the sidecar published in the same release.
sidecar="$(curl -fsSL "$url.sha256")"
expected="$(printf '%s' "$sidecar" | awk '{print $1}' | tr '[:upper:]' '[:lower:]')"
got="$(sha256sum "$zip" | awk '{print $1}')"
if [ "$expected" != "$got" ]; then
    echo "error: checksum mismatch: expected $expected, got $got" >&2
    exit 1
fi
echo "sha256: OK"

mkdir -p "$BIN_DIR"
if command -v unzip >/dev/null 2>&1; then
    unzip -qo "$zip" -d "$tmp"
else
    # Fall back to PowerShell when unzip is missing.
    powershell.exe -NoProfile -Command "Expand-Archive -LiteralPath '$zip' -DestinationPath '$tmp' -Force" \
        || { echo "error: need unzip (or PowerShell) to extract the archive" >&2; exit 1; }
fi
rm -f "$BIN_DIR/chef.old"
if [ -e "$BIN_DIR/chef.exe" ]; then
    mv -f "$BIN_DIR/chef.exe" "$BIN_DIR/chef.old"
fi
mv -f "$tmp/chef.exe" "$BIN_DIR/chef.exe"

win_path="$(command -v cygpath >/dev/null 2>&1 && cygpath -w "$BIN_DIR" || true)"
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        echo ""
        echo "add chef to your PATH (Git Bash: append this to ~/.bashrc):"
        echo "  export PATH=\"\$PATH:$BIN_DIR\""
        if [ -n "$win_path" ]; then
            echo "Windows PATH (cmd/PowerShell, so chef works outside bash):"
            echo "  setx PATH \"%PATH%;$win_path\""
        fi
        ;;
esac

"$BIN_DIR/chef.exe" --version