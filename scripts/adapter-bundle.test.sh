#!/usr/bin/env sh
# Tests for scripts/adapter-bundle.sh. Bundles the pinned adapter into a temp dir, then
# checks that the SDK's per-platform binaries were left out, that the widget patch was
# applied, and that the patched adapter announces the widget capability in its `initialize`
# answer. Needs node, npm, patch and jq; no Claude Code login.
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

platforms="$(find "$out/node_modules/@anthropic-ai" -maxdepth 1 -name 'claude-agent-sdk-*' 2>/dev/null | wc -l | tr -d ' ')"
check "no per-platform SDK binary is bundled" "$platforms" "0"

has() { if grep -q "$1" "$2"; then echo yes; else echo no; fi; }
check "the agent serves _partitur/widget_tools" "$(has '_partitur/widget_tools' "$pkg/dist/v2/agent.js")" "yes"
check "the agent serves _partitur/read_widget" "$(has '_partitur/read_widget' "$pkg/dist/v2/agent.js")" "yes"
check "tool results carry the call result" "$(has 'callToolResult' "$pkg/dist/tool-calls/renderer.js")" "yes"

initialize='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":2,"capabilities":{},"info":{"name":"adapter-bundle-test","version":"0"}}}'
answer="$(printf '%s\n' "$initialize" | timeout 60 node "$pkg/dist/index.js" 2>/dev/null | head -n 1)"
check "initialize announces the widgets" "$(printf '%s' "$answer" | jq -r '.result._meta.partitur.widgets')" "1"

# A patch that no longer applies exactly makes the script fail.
broken="$(mktemp -d)"
mkdir -p "$broken/packaging/adapter" "$broken/scripts" "$broken/crates/partitur-backend-acp/src"
cp "$script" "$broken/scripts/"
cp "$root/crates/partitur-backend-acp/src/runtime.rs" "$broken/crates/partitur-backend-acp/src/"
# One changed context line: without fuzz, its hunk no longer applies.
sed 's/^\( *protocolVersion: v2.PROTOCOL_VERSION,\)$/\1 \/\/ changed/' \
  "$root/packaging/adapter/claude-agent-acp.patch" > "$broken/packaging/adapter/claude-agent-acp.patch"
if sh "$broken/scripts/adapter-bundle.sh" "$broken/out" >/dev/null 2>&1; then status=0; else status=1; fi
rm -rf "$broken"
check "a patch that does not apply fails the bundle" "$status" "1"

if [ "$fail" -eq 0 ]; then echo ok; fi
exit "$fail"
