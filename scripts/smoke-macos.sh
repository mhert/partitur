#!/usr/bin/env sh
# Prove a built `Partitur.app` is signed, carries the patched adapter, starts, opens its window
# and finds the login shell's environment on macOS.
#
# Usage: scripts/smoke-macos.sh <Partitur.app> [expected-version]
#
# The bundle's signature is verified first. With a second argument, `--version` must print
# exactly `partitur <expected-version>`; without one it is only printed. The adapter in
# Contents/Resources/adapter is then run with the Node.js it carries by
# scripts/adapter-initialize.mjs, and its answer to `initialize` must announce the widgets; that
# needs no Claude Code, so the check runs whether or not one is installed. The window run is
# `partitur --smoke`, which opens the real window on throwaway configuration and data folders
# and exits after the first frame. Its stdin is /dev/null, so it starts as from the Finder: with
# no terminal it asks the login shell for its environment, and in smoke mode it waits for that
# lookup before it exits. The run passes only if it exits with status 0 and its log says the
# environment came from the login shell; one from the fallback folders fails it.
#
# Written for macOS's own /bin/sh and BSD tools. macOS has no `timeout`, so the run is started
# in the background and killed after 30 s. It draws in software (ICED_BACKEND=tiny-skia): the
# machines this runs on have no usable GPU.
set -eu

app="${1:?usage: $0 <Partitur.app> [expected-version]}"
expected="${2:-}"
bin="$app/Contents/MacOS/partitur"

if ! codesign --verify --strict --verbose=2 "$app"; then
  echo "smoke: $app does not pass codesign --verify --strict" >&2
  exit 1
fi
echo "smoke ok: the signature verifies"

version="$("$bin" --version)"
echo "$version"
if [ -n "$expected" ] && [ "$version" != "partitur $expected" ]; then
  echo "smoke: $bin reports '$version', expected 'partitur $expected'" >&2
  exit 1
fi
if [ -n "$expected" ]; then
  echo "smoke ok: --version is $expected"
else
  echo "smoke ok: --version runs (no version expected)"
fi

adapter="$app/Contents/Resources/adapter"
if [ ! -x "$adapter/node/bin/node" ]; then
  echo "smoke: $adapter carries no node/bin/node" >&2
  exit 1
fi
if command -v claude >/dev/null 2>&1; then
  echo "smoke: Claude Code is installed; the adapter check does not use it"
else
  echo "smoke: Claude Code is not installed; the adapter check does not need it"
fi
if ! "$adapter/node/bin/node" "$(dirname "$0")/adapter-initialize.mjs" "$adapter"; then
  echo "smoke: the adapter in $adapter failed its initialize check" >&2
  exit 1
fi
echo "smoke ok: the adapter announces the widgets"

work="$(mktemp -d)"
log="$work/smoke.log"
pid=""
cleanup() {
  if [ -n "$pid" ]; then
    kill "$pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 1' INT TERM

# Debug logging so a failed start says where it stopped.
RUST_LOG=partitur=debug ICED_BACKEND=tiny-skia "$bin" --smoke </dev/null 2>"$log" &
pid=$!

# A tenth of a second per round, 300 rounds: the 30 s bound smoke.sh gets from `timeout`.
waited=0
while kill -0 "$pid" 2>/dev/null; do
  if [ "$waited" -ge 300 ]; then
    kill -9 "$pid" 2>/dev/null || true
    echo "smoke: $bin --smoke did not exit within 30 s" >&2
    break
  fi
  sleep 0.1
  waited=$((waited + 1))
done
status=0
wait "$pid" || status=$?
pid=""

echo "--- partitur --smoke log ---" >&2
cat "$log" >&2
echo "--- end of log ---" >&2

if [ "$status" -ne 0 ]; then
  echo "smoke: $bin --smoke exited with status $status" >&2
  exit 1
fi
echo "smoke ok: the window opened and closed"

if ! grep -qF 'process environment from the login shell' "$log"; then
  echo "smoke: $bin did not take its environment from the login shell" >&2
  exit 1
fi
echo "smoke ok: the environment came from the login shell"
