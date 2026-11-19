#!/usr/bin/env sh
# Bundles the pinned Claude Code ACP adapter with Partitur's widget patch.
#
# Usage: adapter-bundle.sh <out-dir> [--with-node <node-dist-url-or-dir>]
#
# Installs the adapter named by `ADAPTER_PACKAGE` in crates/partitur-backend-acp/src/runtime.rs
# (the one pin) under <out-dir>/node_modules, without the Claude Agent SDK's per-platform
# binaries: the adapter runs the installed `claude` through CLAUDE_CODE_EXECUTABLE. It then
# applies packaging/adapter/claude-agent-acp.patch to the package without fuzz and fails if
# any hunk does not apply exactly. With --with-node it also puts a Node.js distribution under
# <out-dir>/node (node/bin/node on Unix, node/node.exe on Windows), taken from an unpacked
# distribution folder or downloaded from a .tar.gz, .tar.xz or .zip URL.
set -eu

usage() { echo "usage: $0 <out-dir> [--with-node <node-dist-url-or-dir>]" >&2; exit 2; }

[ "$#" -ge 1 ] || usage
out="$1"; shift
node_dist=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --with-node) [ "$#" -ge 2 ] || usage; node_dist="$2"; shift 2 ;;
    *) usage ;;
  esac
done

root="$(cd "$(dirname "$0")/.." && pwd)"
runtime="$root/crates/partitur-backend-acp/src/runtime.rs"
patch_file="$root/packaging/adapter/claude-agent-acp.patch"

package="$(sed -n 's/^pub const ADAPTER_PACKAGE: &str = "\(.*\)";$/\1/p' "$runtime")"
if [ -z "$package" ]; then
  echo "adapter-bundle: no ADAPTER_PACKAGE in $runtime" >&2
  exit 1
fi

mkdir -p "$out"
out="$(cd "$out" && pwd)"
# A fresh install every time: npm keeps an installed package as it is, patched or not.
rm -rf "$out/node_modules" "$out/package.json" "$out/package-lock.json"

npm install --omit=optional --no-audit --no-fund --prefix "$out" "$package"

pkg="$out/node_modules/@agentclientprotocol/claude-agent-acp"
if ! patch -p1 -F0 -N -t -d "$pkg" -i "$patch_file"; then
  echo "adapter-bundle: $patch_file does not apply cleanly to $package" >&2
  exit 1
fi

if [ -n "$node_dist" ]; then
  rm -rf "$out/node"
  if [ -d "$node_dist" ]; then
    cp -R "$node_dist" "$out/node"
  else
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    case "$node_dist" in
      *.zip) archive="$work/node.zip" ;;
      *.tar.xz) archive="$work/node.tar.xz" ;;
      *.tar.gz | *.tgz) archive="$work/node.tar.gz" ;;
      *) echo "adapter-bundle: unknown Node.js archive $node_dist" >&2; exit 1 ;;
    esac
    curl -fsSL -o "$archive" "$node_dist"
    mkdir "$work/unpacked"
    case "$archive" in
      *.zip) unzip -q "$archive" -d "$work/unpacked" ;;
      *) tar -xf "$archive" -C "$work/unpacked" ;;
    esac
    # A Node.js distribution unpacks into one folder named after its version and platform.
    top="$(find "$work/unpacked" -mindepth 1 -maxdepth 1 -type d)"
    if [ "$(printf '%s\n' "$top" | wc -l | tr -d ' ')" != 1 ]; then
      echo "adapter-bundle: $node_dist does not unpack into one folder" >&2
      exit 1
    fi
    mv "$top" "$out/node"
  fi
  if [ ! -f "$out/node/bin/node" ] && [ ! -f "$out/node/node.exe" ]; then
    echo "adapter-bundle: no node/bin/node or node/node.exe in $node_dist" >&2
    exit 1
  fi
fi
