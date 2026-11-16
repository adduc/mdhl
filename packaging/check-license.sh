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

# One top-level AND-ed term per line, in a canonical form: parentheses that
# wrap a whole expression are dropped, and the operands of every AND and OR
# are sorted, so "(MIT OR Apache-2.0)" and "Apache-2.0 OR MIT" compare equal.
# AND binds tighter than OR, as in SPDX, and only operators outside
# parentheses split an expression. Old-style "MIT/Apache-2.0" is read as
# "MIT OR Apache-2.0".
terms() {
    sed 's| */ *| OR |g' | awk '
        function trim(s) {
            sub(/^ +/, "", s)
            sub(/ +$/, "", s)
            return s
        }
        # Drop parentheses that wrap all of s, however many layers.
        function unwrap(s,    n, i, c, depth) {
            s = trim(s)
            while (substr(s, 1, 1) == "(") {
                n = length(s)
                depth = 0
                for (i = 1; i <= n; i++) {
                    c = substr(s, i, 1)
                    if (c == "(") depth++
                    else if (c == ")" && --depth == 0) break
                }
                if (i != n) break
                s = trim(substr(s, 2, n - 2))
            }
            return s
        }
        # Split s into parts at each " op " outside parentheses.
        function split_top(s, op, parts,    n, i, c, depth, start, sep) {
            split("", parts)
            sep = " " op " "
            n = 0
            depth = 0
            start = 1
            for (i = 1; i <= length(s); i++) {
                c = substr(s, i, 1)
                if (c == "(") depth++
                else if (c == ")") depth--
                else if (depth == 0 && substr(s, i, length(sep)) == sep) {
                    parts[++n] = substr(s, start, i - start)
                    start = i + length(sep)
                    i = start - 1
                }
            }
            parts[++n] = substr(s, start)
            return n
        }
        # Normalize each part, sort them, and join them with op.
        function join_sorted(parts, n, op,    i, j, t, out) {
            for (i = 1; i <= n; i++) {
                parts[i] = norm(parts[i])
                if (parts[i] ~ / /) parts[i] = "(" parts[i] ")"
            }
            for (i = 2; i <= n; i++)
                for (j = i; j > 1 && parts[j - 1] > parts[j]; j--) {
                    t = parts[j]; parts[j] = parts[j - 1]; parts[j - 1] = t
                }
            out = parts[1]
            for (i = 2; i <= n; i++) out = out " " op " " parts[i]
            return out
        }
        function norm(s,    parts, n) {
            s = unwrap(s)
            if ((n = split_top(s, "OR", parts)) > 1) return join_sorted(parts, n, "OR")
            if ((n = split_top(s, "AND", parts)) > 1) return join_sorted(parts, n, "AND")
            return s
        }
        {
            e = unwrap($0)
            if (e == "") next
            if (split_top(e, "OR", parts) > 1) {
                print norm(e)
                next
            }
            n = split_top(e, "AND", parts)
            for (i = 1; i <= n; i++) print norm(parts[i])
        }
    ' | sort -u
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
