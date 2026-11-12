#!/usr/bin/env bash
# Builds the native crash handler for this machine and checks the report it writes for a
# SIGSEGV and an abort(), and that the signal still reaches the previous handler (the process
# dies with it). Needs a C compiler; Linux only.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
src="$here/../../main/cpp"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
cc -O1 -g -Wall -Wextra -Werror -Wl,--build-id -I "$src" "$here/crash_test.c" "$src/vaka_crash.c" -o "$out/crash_test"
build_id="$(readelf -n "$out/crash_test" | awk '/Build ID/ {print $3}')"

check() { # kind expected_signal expected_exit
  local dir="$out/r$1"
  mkdir -p "$dir"
  set +e; "$out/crash_test" "$dir" "$1" 2>/dev/null; local code=$?; set -e
  [ "$code" = "$3" ] || { echo "kind $1: exit $code, want $3"; exit 1; }
  local f; f="$(ls "$dir"/*.crash)"
  grep -qx "signal $2" "$f" || { echo "kind $1: no 'signal $2'"; cat "$f"; exit 1; }
  grep -qx "session 11111111-2222-3333-4444-555555555555" "$f"
  grep -qx "release 1.2.3" "$f"
  [ "$(grep -c '^frame 0x' "$f")" -ge 3 ] || { echo "kind $1: too few frames"; cat "$f"; exit 1; }
  grep -q "^image 0x[0-9a-f]* 0x[0-9a-f]* $build_id " "$f" || { echo "kind $1: app image missing"; cat "$f"; exit 1; }
  tail -n1 "$f" | grep -qx end
  echo "kind $1: ok ($(grep -c '^frame' "$f") frames, $(grep -c '^image' "$f") images)"
}
check 0 11 139
check 1 6 134
