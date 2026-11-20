#!/usr/bin/env sh
# Tests for scripts/cef-runtime.sh, against stand-in CEF folders: the Linux and the Windows
# runtime are copied with their locales and CREDITS.html, the build-only files are left out, and
# a folder without a core library or a required file fails. Needs no real CEF.
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
script="$root/scripts/cef-runtime.sh"
fail=0

check() {
  desc="$1"; got="$2"; want="$3"
  if [ "$got" = "$want" ]; then echo "ok   - $desc"
  else echo "FAIL - $desc (got '$got', want '$want')"; fail=1; fi
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A stand-in distribution: every name in $1 as an empty file, two locales, and the build-only
# parts export-cef-dir leaves beside them.
fake() {
  dir="$work/$2"
  mkdir -p "$dir/locales" "$dir/include" "$dir/cmake" "$dir/libcef_dll"
  for name in $1; do : > "$dir/$name"; done
  : > "$dir/locales/en-US.pak"
  : > "$dir/locales/de.pak"
  : > "$dir/include/cef_app.h"
  : > "$dir/CMakeLists.txt"
  : > "$dir/archive.json"
  : > "$dir/CREDITS.html"
  echo "$dir"
}

listing() { (cd "$1" && find . -type f | LC_ALL=C sort | tr '\n' ' '); }

linux="$(fake "libcef.so icudtl.dat v8_context_snapshot.bin chrome_100_percent.pak chrome_200_percent.pak resources.pak libvk_swiftshader.so libvulkan.so.1 vk_swiftshader_icd.json chrome-sandbox" linux)"
sh "$script" "$linux" "$work/linux-out" 2>/dev/null
check "the Linux runtime is staged without chrome-sandbox or build files" "$(listing "$work/linux-out")" \
  "./CREDITS.html ./chrome_100_percent.pak ./chrome_200_percent.pak ./icudtl.dat ./libcef.so ./libvk_swiftshader.so ./libvulkan.so.1 ./locales/de.pak ./locales/en-US.pak ./resources.pak ./v8_context_snapshot.bin ./vk_swiftshader_icd.json "

windows="$(fake "libcef.dll chrome_elf.dll icudtl.dat v8_context_snapshot.bin chrome_100_percent.pak chrome_200_percent.pak resources.pak d3dcompiler_47.dll libEGL.dll libGLESv2.dll vk_swiftshader.dll vk_swiftshader_icd.json vulkan-1.dll libcef.lib bootstrap.exe bootstrapc.exe" windows)"
sh "$script" "$windows" "$work/windows-out"
check "the Windows runtime is staged without import libraries or the bootstrap" "$(listing "$work/windows-out")" \
  "./CREDITS.html ./chrome_100_percent.pak ./chrome_200_percent.pak ./chrome_elf.dll ./d3dcompiler_47.dll ./icudtl.dat ./libEGL.dll ./libGLESv2.dll ./libcef.dll ./locales/de.pak ./locales/en-US.pak ./resources.pak ./v8_context_snapshot.bin ./vk_swiftshader.dll ./vk_swiftshader_icd.json ./vulkan-1.dll "

none="$(fake "icudtl.dat" none)"
if sh "$script" "$none" "$work/none-out" 2>"$work/stderr"; then status=0; else status=1; fi
check "a folder without libcef fails" "$status" "1"
check "the failure names the missing library" "$(grep -c 'neither libcef.so nor libcef.dll' "$work/stderr")" "1"

partial="$(fake "libcef.so icudtl.dat chrome_100_percent.pak chrome_200_percent.pak resources.pak" partial)"
if sh "$script" "$partial" "$work/partial-out" 2>"$work/stderr"; then status=0; else status=1; fi
check "a folder missing a required file fails" "$status" "1"
check "the failure names the missing file" "$(grep -c 'misses v8_context_snapshot.bin' "$work/stderr")" "1"

if [ "$fail" -eq 0 ]; then echo ok; fi
exit "$fail"
