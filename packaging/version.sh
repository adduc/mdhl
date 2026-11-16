#!/usr/bin/env bash
# Print the package version from Cargo.toml.
#
#   packaging/version.sh         the Cargo version, e.g. 0.2.0-rc.1
#   packaging/version.sh --rpm   the same version as RPM spells it, 0.2.0~rc.1
#   packaging/version.sh --rpm-release
#                                that and the spec's Release:, 0.2.0~rc.1-1,
#                                without the dist tag
#
# Every script and workflow reads the version through this, so the parsing
# lives in one place.
#
# RPM doesn't allow '-' in a version. A semver pre-release suffix becomes
# '~', which RPM sorts before the final release (0.2.0~rc.1 < 0.2.0), and
# any '-' inside the suffix becomes '_'.
set -euo pipefail

cd "$(dirname "$0")/.."

version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)
if [ -z "$version" ]; then
    echo "no version found in Cargo.toml" >&2
    exit 1
fi

case ${1:-} in
    "") ;;
    --rpm | --rpm-release)
        case $version in
            *-*)
                pre=${version#*-}
                version=${version%%-*}~${pre//-/_}
                ;;
        esac
        if [ "$1" = --rpm-release ]; then
            release=$(sed -n 's/^Release:[[:space:]]*//p' packaging/mdhl.spec |
                head -n1 | sed 's/[[:space:]]*$//')
            release=${release%%%\{?dist\}}
            if [ -z "$release" ]; then
                echo "no Release: found in packaging/mdhl.spec" >&2
                exit 1
            fi
            version=$version-$release
        fi
        ;;
    *)
        echo "usage: $0 [--rpm | --rpm-release]" >&2
        exit 2
        ;;
esac

echo "$version"
