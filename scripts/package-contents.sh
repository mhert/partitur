#!/usr/bin/env sh
# Checks that a built package carries the files widgets need and none it must not.
#
# Usage: package-contents.sh <package> < required-paths
#
# <package> is a .deb, an Arch .pkg.tar.zst, a .tar.gz, a .zip or an unpacked folder (an
# extracted AppImage, say). Standard input names one path per line, relative to the package's
# root; blank lines are skipped. The check fails when a named path is missing, when any path
# lies in a `claude-agent-sdk-<platform>` package (the Claude Agent SDK's per-platform Claude Code
# binary, which the adapter bundle leaves out: the adapter runs the installed `claude`), or when
# the package holds a chrome-sandbox (CEF sandboxes in user namespaces instead). It prints the
# package's size in files and every problem it finds.
set -eu

[ "$#" -eq 1 ] || { echo "usage: $0 <package> < required-paths" >&2; exit 2; }
package="$1"

case "$package" in
  *.deb) listing="$(dpkg-deb --fsys-tarfile "$package" | tar -t)" ;;
  *.pkg.tar.zst) listing="$(tar --zstd -tf "$package")" ;;
  *.tar.gz | *.tgz) listing="$(tar -tzf "$package")" ;;
  *.zip) listing="$(unzip -Z1 "$package")" ;;
  *)
    if [ -d "$package" ]; then
      listing="$(cd "$package" && find .)"
    else
      echo "package-contents: $package is no package this script reads" >&2
      exit 2
    fi
    ;;
esac
# One path per line, without a leading ./ or a trailing / and with forward slashes.
# shellcheck disable=SC1003 # '\\' is tr's backslash, not an escaped quote.
paths="$(printf '%s\n' "$listing" | tr '\\' '/' | sed 's|^\./||; s|/$||' | grep -v '^\.\{0,1\}$' || true)"
echo "package-contents: $package holds $(printf '%s\n' "$paths" | wc -l | tr -d ' ') paths"

fail=0
while IFS= read -r want; do
  want="$(printf '%s' "$want" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
  [ -n "$want" ] || continue
  if ! printf '%s\n' "$paths" | grep -qxF "$want"; then
    echo "package-contents: $package misses $want" >&2
    fail=1
  fi
done

sdk="$(printf '%s\n' "$paths" | grep 'claude-agent-sdk-' || true)"
if [ -n "$sdk" ]; then
  echo "package-contents: $package holds a per-platform Claude Agent SDK package:" >&2
  printf '%s\n' "$sdk" | head -n 5 >&2
  fail=1
fi
sandbox="$(printf '%s\n' "$paths" | grep '\(^\|/\)chrome-sandbox$' || true)"
if [ -n "$sandbox" ]; then
  echo "package-contents: $package holds $sandbox" >&2
  fail=1
fi

if [ "$fail" -eq 0 ]; then echo "package-contents ok: $package"; fi
exit "$fail"
