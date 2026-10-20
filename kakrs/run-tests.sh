#!/bin/sh
# Run the upstream integration test suite (../test) against the Rust binary.
# Usage: ./run-tests.sh [test dirs relative to ../test]...
set -e
here=$(cd "$(dirname "$0")" && pwd)
cargo build --manifest-path "$here/Cargo.toml" ${CARGO_FLAGS} >&2
kak="$here/target/${PROFILE:-debug}/kak"
runner=$(mktemp "${TMPDIR:-/tmp}/kakrs-run.XXXXXX")
trap 'rm -f "$runner"' EXIT
sed "s|kak=\"\$(realpath \$testdir)/../src/kak\"|kak=\"$kak\"|" "$here/../test/run" > "$runner"
cd "$here/../test"
if [ $# -eq 0 ]; then set -- $(ls -d */ | sed 's|/$||'); fi
sh "$runner" "$@"
