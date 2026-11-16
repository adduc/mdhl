#!/usr/bin/env bash
# Check that Cargo.toml and the spec file carry the same version, and that
# the newest %changelog entry is for that version and release. With a tag
# argument (e.g. v1.2.0), check that it matches too.
#
# A pre-release such as 0.2.0-rc.1 is written 0.2.0~rc.1 in the spec, since
# RPM doesn't allow '-' in a version (see packaging/version.sh). The tag
# keeps the Cargo form: v0.2.0-rc.1.
set -euo pipefail

cd "$(dirname "$0")/.."

spec_file=packaging/mdhl.spec

cargo=$(packaging/version.sh)
rpm=$(packaging/version.sh --rpm)
rpm_release=$(packaging/version.sh --rpm-release)
spec=$(sed -n 's/^Version: *//p' "$spec_file" | head -n1)
# The first entry after %changelog ends in "- VERSION-RELEASE".
changelog=$(sed -n '/^%changelog/,$ { /^\*/ { s/.* - *\([^ ]*\) *$/\1/p; q } }' "$spec_file")

status=0
fail() {
    echo "$*" >&2
    status=1
}

if [ "$spec" != "$rpm" ]; then
    if [ "$rpm" != "$cargo" ]; then
        fail "$spec_file has Version '$spec', but Cargo.toml has '$cargo'," \
            "which RPM spells '$rpm'"
    else
        fail "$spec_file has Version '$spec', but Cargo.toml has '$cargo'"
    fi
fi
if [ "$changelog" != "$rpm_release" ]; then
    fail "the newest %changelog entry in $spec_file is for" \
        "'${changelog:-nothing}', but the package is '$rpm_release';" \
        "add an entry for this release"
fi
if [ -n "${1:-}" ] && [ "${1#v}" != "$cargo" ]; then
    fail "tag $1 doesn't match Cargo.toml's version '$cargo'; expected v$cargo"
fi

if [ $status -eq 0 ]; then
    echo "version $cargo (RPM $rpm_release) is consistent"
fi
exit $status
