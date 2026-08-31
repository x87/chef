#!/usr/bin/env bash
# chef one-line installer (Linux, Git Bash, MSYS2, WSL, or any Unix shell).
#   curl -fsSL https://raw.githubusercontent.com/x87/chef/master/install.sh | bash
#
# Picks the native binary for the host platform: Linux gets the linux-gnu
# build, anything else (Git Bash / MSYS2 / WSL on Windows) gets the Windows
# build. Same release asset, same checksum sidecar, same chef.old rollback;
# chef installs into <CHEF_HOME>/bin.
set -euo pipefail

REPO="x87/chef"

# Host platform: native Linux vs Windows (Git Bash / MSYS2 / WSL).
case "$(uname -s)" in
    Linux*)
        BIN="chef"
        ASSET="chef-x86_64-unknown-linux-gnu.zip"
        # Match dirs::data_local_dir on Linux ($XDG_DATA_HOME, else ~/.local/share)
        DEFAULT_HOME="${XDG_DATA_HOME:-$HOME/.local/share}/chef"
        ;;
    *)
        BIN="chef.exe"
        ASSET="chef-x86_64-pc-windows-msvc.zip"
        DEFAULT_HOME="${LOCALAPPDATA:-$HOME/AppData/Local}/Chef"
        ;;
esac

CHEF_HOME="${CHEF_HOME:-$DEFAULT_HOME}"
BIN_DIR="$CHEF_HOME/bin"

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }

echo "resolving latest release of $REPO..."
# GitHub redirects these to the latest release's asset bytes - no API call,
# so installers are immune to api.github.com rate limits (403) and need no auth.
BASE_URL="https://github.com/$REPO/releases/latest/download"
url="$BASE_URL/$ASSET"

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
elif [ "$(uname -s)" = "Linux" ]; then
    echo "error: unzip is required to extract the archive (e.g. apt install unzip)" >&2
    exit 1
elif command -v powershell.exe >/dev/null 2>&1; then
    # Windows fallback when unzip is missing (Git Bash / MSYS2).
    powershell.exe -NoProfile -Command "Expand-Archive -LiteralPath '$zip' -DestinationPath '$tmp' -Force" \
        || { echo "error: need unzip (or PowerShell) to extract the archive" >&2; exit 1; }
else
    echo "error: need unzip to extract the archive" >&2
    exit 1
fi
rm -f "$BIN_DIR/chef.old"
if [ -e "$BIN_DIR/$BIN" ]; then
    mv -f "$BIN_DIR/$BIN" "$BIN_DIR/chef.old"
fi
mv -f "$tmp/$BIN" "$BIN_DIR/$BIN"
# On Linux the release zip may not preserve the executable bit; the file
# must be executable or running it fails ("error 90" / Permission denied).
[ "$(uname -s)" = "Linux" ] && chmod +x "$BIN_DIR/$BIN"

win_path="$(command -v cygpath >/dev/null 2>&1 && cygpath -w "$BIN_DIR" || true)"
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        echo ""
        echo "add chef to your PATH (e.g. append this to ~/.bashrc):"
        echo "  export PATH=\"\$PATH:$BIN_DIR\""
        if [ -n "$win_path" ]; then
            echo "Windows PATH (cmd/PowerShell, so chef works outside bash):"
            echo "  setx PATH \"%PATH%;$win_path\""
        fi
        ;;
esac

"$BIN_DIR/$BIN" --version