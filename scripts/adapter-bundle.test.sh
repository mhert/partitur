#!/usr/bin/env sh
# Tests for scripts/adapter-bundle.sh. Bundles the pinned adapter into a temp dir, then
# checks that it was installed from the committed lockfile, that a lockfile which does not pin
# ADAPTER_PACKAGE fails the bundle, that the SDK's per-platform binaries were left out, that the
# widget patch was
# applied, and that the patched adapter announces the widget capability in its `initialize`
# answer and that the patched helpers behave (scripts/adapter-bundle.test.mjs), and that
# --with-node checks a downloaded Node.js against its SHA-256 and keeps only its executable.
# Needs node, npm, patch, curl and jq; no Claude Code login.
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
script="$root/scripts/adapter-bundle.sh"
fail=0

check() {
  desc="$1"; got="$2"; want="$3"
  if [ "$got" = "$want" ]; then echo "ok   - $desc"
  else echo "FAIL - $desc (got '$got', want '$want')"; fail=1; fi
}

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

if [ ! -f "$script" ]; then
  echo "FAIL - adapter-bundle.sh: not found"
  exit 1
fi
sh "$script" "$out" >/dev/null
pkg="$out/node_modules/@agentclientprotocol/claude-agent-acp"

# The install is the committed lockfile's, to the byte, and that lockfile pins ADAPTER_PACKAGE.
lock="$root/packaging/adapter/package-lock.json"
if cmp -s "$lock" "$out/package-lock.json"; then same=yes; else same=no; fi
check "the bundle is installed from the committed lockfile" "$same" "yes"
installed="$(node -p 'require(process.argv[1]).version' "$pkg/package.json")"
pinned="$(sed -n 's/^pub const ADAPTER_PACKAGE: &str = "@agentclientprotocol\/claude-agent-acp@\(.*\)";$/\1/p' "$root/crates/partitur-backend-acp/src/runtime.rs")"
check "the installed adapter is the pinned one" "$installed" "$pinned"
sdk="$(node -p 'require(process.argv[1]).packages["node_modules/@anthropic-ai/claude-agent-sdk"].version' "$lock")"
check "the SDK is the locked one" "$(node -p 'require(process.argv[1]).version' "$out/node_modules/@anthropic-ai/claude-agent-sdk/package.json")" "$sdk"

# A pin the lockfile does not match fails the bundle before anything is installed.
stale="$(mktemp -d)"
mkdir -p "$stale/packaging/adapter" "$stale/scripts" "$stale/crates/partitur-backend-acp/src"
cp "$script" "$stale/scripts/"
cp "$root/packaging/adapter/claude-agent-acp.patch" "$root/packaging/adapter/package.json" "$lock" "$stale/packaging/adapter/"
sed 's/^\(pub const ADAPTER_PACKAGE: &str = "@agentclientprotocol\/claude-agent-acp@\).*";$/\10.0.1";/' \
  "$root/crates/partitur-backend-acp/src/runtime.rs" > "$stale/crates/partitur-backend-acp/src/runtime.rs"
if sh "$stale/scripts/adapter-bundle.sh" "$stale/out" >/dev/null 2>"$stale/stderr"; then status=0; else status=1; fi
if grep -q "does not pin @agentclientprotocol/claude-agent-acp@0.0.1" "$stale/stderr"; then said=yes; else said=no; fi
if [ -d "$stale/out/node_modules" ]; then installed=yes; else installed=no; fi
rm -rf "$stale"
check "a lockfile that does not pin ADAPTER_PACKAGE fails the bundle" "$status" "1"
check "the failed bundle says the lockfile is stale" "$said" "yes"
check "the failed bundle installs nothing" "$installed" "no"

platforms="$(find "$out/node_modules/@anthropic-ai" -maxdepth 1 -name 'claude-agent-sdk-*' 2>/dev/null | wc -l | tr -d ' ')"
check "no per-platform SDK binary is bundled" "$platforms" "0"

has() { if grep -q "$1" "$2"; then echo yes; else echo no; fi; }
check "the agent serves _partitur/widget_tools" "$(has '_partitur/widget_tools' "$pkg/dist/v2/agent.js")" "yes"
check "the agent serves _partitur/read_widget" "$(has '_partitur/read_widget' "$pkg/dist/v2/agent.js")" "yes"
check "tool results carry the call result" "$(has 'callToolResult' "$pkg/dist/tool-calls/renderer.js")" "yes"

initialize='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":2,"capabilities":{},"info":{"name":"adapter-bundle-test","version":"0"}}}'
answer="$(printf '%s\n' "$initialize" | timeout 60 node "$pkg/dist/index.js" 2>/dev/null | head -n 1)"
check "initialize announces the widgets" "$(printf '%s' "$answer" | jq -r '.result._meta.partitur.widgets')" "1"

# The patched helpers' behaviour, against a stand-in for the session's Claude Code query.
if node "$root/scripts/adapter-bundle.test.mjs" "$pkg"; then :; else fail=1; fi

# A patch that no longer applies exactly makes the script fail.
broken="$(mktemp -d)"
mkdir -p "$broken/packaging/adapter" "$broken/scripts" "$broken/crates/partitur-backend-acp/src"
cp "$script" "$broken/scripts/"
cp "$root/packaging/adapter/package.json" "$root/packaging/adapter/package-lock.json" "$broken/packaging/adapter/"
cp "$root/crates/partitur-backend-acp/src/runtime.rs" "$broken/crates/partitur-backend-acp/src/"
# One changed context line: without fuzz, its hunk no longer applies.
sed 's/^\( *protocolVersion: v2.PROTOCOL_VERSION,\)$/\1 \/\/ changed/' \
  "$root/packaging/adapter/claude-agent-acp.patch" > "$broken/packaging/adapter/claude-agent-acp.patch"
if sh "$broken/scripts/adapter-bundle.sh" "$broken/out" >/dev/null 2>"$broken/stderr"; then status=0; else status=1; fi
if grep -q "does not apply cleanly" "$broken/stderr"; then said=yes; else said=no; fi
rm -rf "$broken"
check "a patch that does not apply fails the bundle" "$status" "1"
check "the failed bundle says the patch does not apply" "$said" "yes"

# --with-node keeps only the executable and its licence of a downloaded distribution, and only
# when the archive has the SHA-256 it was given.
dist="$(mktemp -d)"
mkdir -p "$dist/node-v0-test/bin" "$dist/node-v0-test/include/node" "$dist/node-v0-test/lib/node_modules/npm"
printf '#!/bin/sh\necho fake node\n' > "$dist/node-v0-test/bin/node"
chmod 755 "$dist/node-v0-test/bin/node"
echo licence > "$dist/node-v0-test/LICENSE"
touch "$dist/node-v0-test/bin/npm" "$dist/node-v0-test/include/node/node.h"
tar -czf "$dist/node.tar.gz" -C "$dist" node-v0-test
if command -v sha256sum >/dev/null 2>&1; then sum="$(sha256sum "$dist/node.tar.gz" | cut -d ' ' -f 1)"
else sum="$(shasum -a 256 "$dist/node.tar.gz" | cut -d ' ' -f 1)"; fi
with_node="$dist/out"
if sh "$script" "$with_node" --with-node "file://$dist/node.tar.gz" --node-sha256 "$sum" >/dev/null 2>&1; then status=0; else status=1; fi
check "a Node archive with the given SHA-256 is bundled" "$status" "0"
check "the bundled node runs" "$("$with_node/node/bin/node" 2>/dev/null || true)" "fake node"
check "only node and its licence are kept" "$(cd "$with_node/node" && find . -type f | LC_ALL=C sort | tr '\n' ' ')" "./LICENSE ./bin/node "
wrong="0000000000000000000000000000000000000000000000000000000000000000"
if sh "$script" "$dist/wrong" --with-node "file://$dist/node.tar.gz" --node-sha256 "$wrong" >/dev/null 2>"$dist/stderr"; then status=0; else status=1; fi
check "a Node archive with another SHA-256 fails the bundle" "$status" "1"
if grep -q "expected $wrong" "$dist/stderr"; then said=yes; else said=no; fi
check "the failed bundle names the expected SHA-256" "$said" "yes"
rm -rf "$dist"

if [ "$fail" -eq 0 ]; then echo ok; fi
exit "$fail"
