#!/usr/bin/env sh
# Prove an installed or packed `partitur` starts and opens its window on X11 and on Wayland.
#
# Usage: scripts/smoke.sh <partitur> [expected-version]
#
# The first argument is a path to the binary or a bare command name, so a package installed by
# a package manager and an AppImage in the working directory are smoked the same way. With a
# second argument, `--version` must print exactly `partitur <expected-version>`; without one it
# is only printed. Each window run is `partitur --smoke`, which opens the real window on
# throwaway configuration and data folders and exits after the first frame, bounded by
# `timeout` because the mode has no time limit of its own.
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

# Debug logging so a failed start says where it stopped.
export RUST_LOG=partitur=debug
export ICED_BACKEND=tiny-skia

# X11. Unsetting WAYLAND_DISPLAY is what keeps winit from picking Wayland when the caller
# happens to have a compositor.
env -u WAYLAND_DISPLAY xvfb-run -a timeout 30 "$bin" --smoke
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
  timeout 30 "$bin" --smoke; then
  echo "smoke: $bin --smoke failed on Wayland; weston's log follows" >&2
  cat "$runtime/weston.log" >&2
  exit 1
fi
echo "smoke ok: Wayland (weston headless)"
