#!/usr/bin/env bash
# Check that the spec's License: covers exactly the licenses of mdhl and the
# crates compiled into it, so a dependency update can't leave it stale.
#
# The crates are listed the way %cargo_license_summary lists them: runtime
# dependencies only, for every target. The expected License: is each
# distinct license expression, sorted, parenthesized when it has an
# operator, and joined with AND. The spec must match it exactly; on a
# mismatch the script prints the line to paste in.
set -euo pipefail

cd "$(dirname "$0")/.."

spec_file=packaging/mdhl.spec

crates=$(cargo tree --workspace --locked --edges no-build,no-dev,no-proc-macro \
    --no-dedupe --target all --prefix none --format '{p}|{l}' | sort -u)

# A crate with only a license-file has no SPDX expression to list.
if grep -q '|$' <<< "$crates"; then
    echo "these crates declare no SPDX license; check them by hand:" >&2
    sed -n 's/|$//p' <<< "$crates" >&2
    exit 1
fi

# Old-style "MIT/Apache-2.0" means "MIT OR Apache-2.0".
expected=$(cut -d'|' -f2 <<< "$crates" | sed 's| */ *| OR |g' | LC_ALL=C sort -u |
    awk '{ printf "%s%s", (NR > 1 ? " AND " : ""), (/ / ? "(" $0 ")" : $0) }')
actual=$(sed -n 's/^License:[[:space:]]*//p' "$spec_file" | head -n1 | sed 's/[[:space:]]*$//')

if [ "$actual" != "$expected" ]; then
    echo "License: in $spec_file doesn't match the crates' licenses." >&2
    echo "  spec:     $actual" >&2
    echo "  expected: $expected" >&2
    echo "Crates and their licenses:" >&2
    sed 's/|/: /' <<< "$crates" >&2
    exit 1
fi

echo "License: matches the $(wc -l <<< "$crates") crates' licenses"
