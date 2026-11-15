#!/usr/bin/env bash
# Check that Cargo.toml and the spec file carry the same version. With a tag
# argument (e.g. v1.2.0), check that it matches too.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)
spec=$(sed -n 's/^Version: *//p' packaging/mdhl.spec | head -n1)

status=0
check() {
    if [ "$2" != "$cargo" ]; then
        echo "$1 has version '$2', but Cargo.toml has '$cargo'" >&2
        status=1
    fi
}
check packaging/mdhl.spec "$spec"
if [ -n "${1:-}" ]; then
    check "tag $1" "${1#v}"
fi

if [ $status -eq 0 ]; then
    echo "version $cargo is consistent"
fi
exit $status
