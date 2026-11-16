#!/usr/bin/env bash
# Check that the spec's License: covers exactly the licenses of mdhl and the
# crates compiled into it, so a dependency update can't leave it stale.
#
# The crates are listed the way %cargo_license_summary lists them: runtime
# dependencies only, for every target. Both sides are split into their
# AND-ed terms and compared as sets, so the order in the spec is free.
set -euo pipefail

cd "$(dirname "$0")/.."

spec_file=packaging/mdhl.spec

# One term per line, outer parentheses dropped. Old-style "MIT/Apache-2.0"
# is read as "MIT OR Apache-2.0".
terms() {
    sed -e 's| */ *| OR |g' -e 's/ AND /\n/g' |
        sed -e 's/^ *(\(.*\)) *$/\1/' -e 's/^ *//' -e 's/ *$//' |
        sed '/^$/d' | sort -u
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cargo tree --workspace --locked --edges no-build,no-dev,no-proc-macro \
    --no-dedupe --target all --prefix none --format '{p}|{l}' |
    sort -u > "$tmp/crates"

# A crate with only a license-file has no SPDX expression to compare.
if grep -q '|$' "$tmp/crates"; then
    echo "these crates declare no SPDX license; check them by hand:" >&2
    sed -n 's/|$//p' "$tmp/crates" >&2
    exit 1
fi

cut -d'|' -f2 "$tmp/crates" | terms > "$tmp/crates.terms"
sed -n 's/^License: *//p' "$spec_file" | head -n1 | terms > "$tmp/spec.terms"

if ! diff -u --label "License: in $spec_file" --label "crate licenses" \
    "$tmp/spec.terms" "$tmp/crates.terms"; then
    echo >&2
    echo "Update License: in $spec_file to match (+ add, - remove)." >&2
    echo "Crates and their licenses:" >&2
    sed 's/|/: /' "$tmp/crates" >&2
    exit 1
fi

echo "License: matches the $(wc -l < "$tmp/crates") crates' licenses"
