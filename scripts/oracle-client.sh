#!/usr/bin/env bash
# Install Oracle Instant Client (basiclite, free, no account) for Oracle
# connections. Linux x86_64 only; macOS users take the dmg from
# https://www.oracle.com/database/technologies/instant-client/downloads.html
#
# A zip install has no run path, so libclntsh.so cannot find its own
# libnnz.so and libclntshcore.so unless the loader is told. This script
# writes one with patchelf ($ORIGIN), which is per-user and needs no root.
# Without patchelf, either put the directory in /etc/ld.so.conf.d and run
# ldconfig, or export LD_LIBRARY_PATH.
set -euo pipefail

dest="${1:-$HOME/.local/opt/oracle}"
url="https://download.oracle.com/otn_software/linux/instantclient/instantclient-basiclite-linuxx64.zip"

mkdir -p "$dest"
cd "$dest"
if ! ls -d instantclient_* >/dev/null 2>&1; then
    curl -sSL -o basiclite.zip "$url"
    unzip -qo basiclite.zip && rm basiclite.zip
fi
dir="$(ls -d "$dest"/instantclient_* | sort | tail -1)"

if command -v patchelf >/dev/null; then pe=patchelf
elif command -v uvx >/dev/null; then pe="uvx patchelf"
else pe=""; fi
if [ -n "$pe" ]; then
    for lib in "$dir"/libclntsh.so.*.1 "$dir"/libclntshcore.so.*.1 "$dir"/libnnz.so; do
        [ -f "$lib" ] && $pe --set-rpath '$ORIGIN' "$lib"
    done
    echo "run path set with patchelf"
else
    echo "patchelf not found: add $dir to /etc/ld.so.conf.d and run ldconfig, or export LD_LIBRARY_PATH=$dir"
fi
if ! ldd "$dir/libclntsh.so" | grep -q "libaio.so.1 =>.*/"; then
    echo "libaio is missing: install it (Arch: pacman -S libaio; Debian: apt install libaio1t64)"
fi
echo "Instant Client at $dir — set [oracle] client_lib_dir = \"$dir\" in config.toml"
