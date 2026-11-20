#!/usr/bin/env sh
# Stages the CEF runtime a packaged Partitur loads, out of a CEF distribution folder.
#
# Usage: cef-runtime.sh <cef-dir> <out-dir>
#
# <cef-dir> is a folder as cef-rs's export-cef-dir lays it out (the distribution's Release and
# Resources folders merged), which is also what CEF_PATH points the build at. The files copied
# into <out-dir> are those the distribution's README lists as CEF's runtime: the core library,
# icudtl.dat, v8_context_snapshot.bin, the three .pak files and locales/, and the software
# renderer (SwiftShader, and on Windows ANGLE and the DirectX compilers) where the distribution
# has it. Neither chrome-sandbox nor the headers, the cmake files, the libcef_dll sources, the
# import libraries or the Windows sandbox bootstrap are copied; CREDITS.html, the licences of
# Chromium and its parts, is. The platform is told by the core library found: libcef.so for
# Linux, libcef.dll for Windows. On Linux the shared libraries are stripped of their debug
# symbols when `strip` is on the PATH, which takes libcef.so from about 1.5 GB to about 0.3 GB.
# Fails when the folder holds no core library or misses a required file.
set -eu

[ "$#" -eq 2 ] || { echo "usage: $0 <cef-dir> <out-dir>" >&2; exit 2; }
cef="$1"
out="$2"

if [ -f "$cef/libcef.so" ]; then
  required="libcef.so icudtl.dat v8_context_snapshot.bin chrome_100_percent.pak chrome_200_percent.pak resources.pak"
  optional="libvk_swiftshader.so libvulkan.so.1 vk_swiftshader_icd.json libEGL.so libGLESv2.so"
elif [ -f "$cef/libcef.dll" ]; then
  required="libcef.dll chrome_elf.dll icudtl.dat v8_context_snapshot.bin chrome_100_percent.pak chrome_200_percent.pak resources.pak"
  optional="d3dcompiler_47.dll dxcompiler.dll dxil.dll libEGL.dll libGLESv2.dll vk_swiftshader.dll vk_swiftshader_icd.json vulkan-1.dll"
else
  echo "cef-runtime: $cef holds neither libcef.so nor libcef.dll" >&2
  exit 1
fi

for name in $required; do
  if [ ! -f "$cef/$name" ]; then
    echo "cef-runtime: $cef misses $name" >&2
    exit 1
  fi
done
if [ ! -d "$cef/locales" ]; then
  echo "cef-runtime: $cef misses locales/" >&2
  exit 1
fi

mkdir -p "$out/locales"
for name in $required $optional CREDITS.html; do
  if [ -f "$cef/$name" ]; then
    cp "$cef/$name" "$out/$name"
  fi
done
cp "$cef/locales/"*.pak "$out/locales/"

if [ -f "$out/libcef.so" ] && command -v strip >/dev/null 2>&1; then
  for lib in "$out"/*.so "$out"/*.so.*; do
    if [ -f "$lib" ]; then
      if ! strip --strip-unneeded "$lib" 2>/dev/null; then
        echo "cef-runtime: $lib could not be stripped and is staged as it is" >&2
      fi
    fi
  done
fi
