#!/usr/bin/env bash
# Print the package version from Cargo.toml.
#
#   packaging/version.sh         the Cargo version, e.g. 0.2.0-rc.1
#   packaging/version.sh --rpm   the same version as RPM spells it, 0.2.0~rc.1
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

if [ "${1:-}" = --rpm ]; then
    case $version in
        *-*)
            pre=${version#*-}
            version=${version%%-*}~${pre//-/_}
            ;;
    esac
fi

echo "$version"
