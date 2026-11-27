#!/usr/bin/env sh
# Prove an installed or packed `partitur` starts and opens its window on X11 and on Wayland, and
# that the adapter it carries is the patched one.
#
# Usage: scripts/smoke.sh <partitur> [expected-version]
#
# The first argument is a path to the binary or a bare command name, so a package installed by
# a package manager and an AppImage in the working directory are smoked the same way. With a
# second argument, `--version` must print exactly `partitur <expected-version>`; without one it
# is only printed. The adapter is looked for where the binary finds it, in ../lib/partitur/adapter
# from the binary's real folder (for an AppImage, inside its extracted payload), and is run with
# its own Node.js, or the one on the PATH when it carries none, by scripts/adapter-initialize.mjs:
# its answer to `initialize` must announce the widgets. That needs no Claude Code, so the check
# runs whether or not one is installed. Each window run is `partitur --smoke`, which opens the real window on
# throwaway configuration and data folders and exits after the first frame, bounded by
# `timeout` because the mode has no time limit of its own. Its log on stderr is printed after
# the run, and the run fails unless it says `the widget engine started`: that proves the packaged
# widget engine helper is found and starts. With PARTITUR_SMOKE_SANDBOX=1 it must also say
# `sandboxed=true`, which proves CEF's process sandbox works; leave it unset where the host may
# refuse the user namespaces the sandbox needs (the helper then runs unsandboxed after a warning).
#
# Needs xvfb-run (for X11) and weston (for Wayland) on the PATH. Both runs draw in software
# (ICED_BACKEND=tiny-skia): the machines this runs on have no GPU.
set -eu

bin="${1:?usage: $0 <partitur> [expected-version]}"
expected="${2:-}"

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

# The adapter. An AppImage is unpacked to reach it; anything else is followed to its real path.
scripts="$(cd "$(dirname "$0")" && pwd)"
unpacked=""
case "$bin" in
  *.AppImage)
    unpacked="$(mktemp -d)"
    appimage="$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")"
    (cd "$unpacked" && "$appimage" --appimage-extract >/dev/null)
    adapter="$unpacked/squashfs-root/usr/lib/partitur/adapter"
    ;;
  *)
    real="$(readlink -f "$(command -v "$bin")")"
    adapter="$(dirname "$real")/../lib/partitur/adapter"
    ;;
esac
if [ -x "$adapter/node/bin/node" ]; then
  node="$adapter/node/bin/node"
elif command -v node >/dev/null 2>&1; then
  node="node"
else
  echo "smoke: $adapter carries no Node.js and none is on the PATH" >&2
  exit 1
fi
if command -v claude >/dev/null 2>&1; then
  echo "smoke: Claude Code is installed; the adapter check does not use it"
else
  echo "smoke: Claude Code is not installed; the adapter check does not need it"
fi
status=0
"$node" "$scripts/adapter-initialize.mjs" "$adapter" || status=$?
if [ -n "$unpacked" ]; then rm -rf "$unpacked"; fi
if [ "$status" -ne 0 ]; then
  echo "smoke: the adapter in $adapter failed its initialize check" >&2
  exit 1
fi
echo "smoke ok: the adapter announces the widgets"

# Debug logging so a failed start says where it stopped.
export RUST_LOG=partitur=debug
export ICED_BACKEND=tiny-skia

# Prints the log of a `--smoke` run, then fails unless it shows the widget engine started (and
# sandboxed, when PARTITUR_SMOKE_SANDBOX=1).
check_engine() {
  echo "--- partitur --smoke log ($2) ---" >&2
  cat "$1" >&2
  echo "--- end of log ---" >&2
  if ! grep -qF 'the widget engine started' "$1"; then
    echo "smoke: the widget engine did not start on $2" >&2
    exit 1
  fi
  if [ "${PARTITUR_SMOKE_SANDBOX:-}" = 1 ] && ! grep -F 'the widget engine started' "$1" | grep -qF 'sandboxed=true'; then
    echo "smoke: the widget engine started without its sandbox on $2" >&2
    exit 1
  fi
  echo "smoke ok: the widget engine starts on $2"
}

# X11. Unsetting WAYLAND_DISPLAY is what keeps winit from picking Wayland when the caller
# happens to have a compositor. Ubuntu's xvfb-run hands the program's standard error on as its
# standard output, so the log takes both.
x11_log="$(mktemp)"
if ! env -u WAYLAND_DISPLAY xvfb-run -a timeout 30 "$bin" --smoke >"$x11_log" 2>&1; then
  cat "$x11_log" >&2
  rm -f "$x11_log"
  echo "smoke: $bin --smoke failed on X11 (Xvfb)" >&2
  exit 1
fi
check_engine "$x11_log" "X11"
rm -f "$x11_log"
echo "smoke ok: X11 (Xvfb)"

# Wayland, under weston's headless backend in a runtime directory of its own, so neither the
# socket nor weston's lock file meets anything the caller already runs.
runtime="$(mktemp -d)"
chmod 700 "$runtime"
weston_pid=""
cleanup() {
  if [ -n "$weston_pid" ]; then
    kill "$weston_pid" 2>/dev/null || true
    wait "$weston_pid" 2>/dev/null || true
  fi
  rm -rf "$runtime"
}
trap cleanup EXIT
trap 'exit 1' INT TERM

# Weston 10 renamed its backends: before it only the module file name is understood (Ubuntu
# 22.04 ships weston 9), from it on the short name is the documented one.
major="$(weston --version 2>/dev/null | sed -n 's/^weston \([0-9][0-9]*\)\..*/\1/p')"
if [ -n "$major" ] && [ "$major" -lt 10 ]; then
  backend=headless-backend.so
else
  backend=headless
fi

XDG_RUNTIME_DIR="$runtime" weston --backend="$backend" --socket=partitur-smoke \
  --idle-time=0 >"$runtime/weston.log" 2>&1 &
weston_pid=$!

waited=0
while [ ! -S "$runtime/partitur-smoke" ]; do
  if ! kill -0 "$weston_pid" 2>/dev/null || [ "$waited" -ge 100 ]; then
    echo "smoke: weston did not open its socket within 10 s" >&2
    cat "$runtime/weston.log" >&2
    exit 1
  fi
  sleep 0.1
  waited=$((waited + 1))
done

if ! env -u DISPLAY XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=partitur-smoke \
  timeout 30 "$bin" --smoke 2>"$runtime/partitur.log"; then
  cat "$runtime/partitur.log" >&2
  echo "smoke: $bin --smoke failed on Wayland; weston's log follows" >&2
  cat "$runtime/weston.log" >&2
  exit 1
fi
check_engine "$runtime/partitur.log" "Wayland" || exit 1
echo "smoke ok: Wayland (weston headless)"
