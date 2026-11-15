#!/usr/bin/env bash
# Build a statically linked Linux binary and pack it, with the README and
# license, as target/dist/mdhl-VERSION-ARCH-linux.tar.gz.
#
#   packaging/build-tarball.sh [TARGET]
#
# TARGET defaults to the host's musl target, e.g. x86_64-unknown-linux-musl;
# add it first with `rustup target add`. musl links the C runtime into the
# binary, so it runs on any Linux distribution regardless of its glibc.
set -euo pipefail

cd "$(dirname "$0")/.."

name=mdhl
version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)
target=${1:-$(uname -m)-unknown-linux-musl}
arch=${target%%-*}
dist=target/dist
stage=$name-$version-$arch-linux

cargo build --release --locked --target "$target"

rm -rf "${dist:?}/$stage" "$dist/$stage.tar.gz"
mkdir -p "$dist/$stage"
install -m755 "target/$target/release/$name" "$dist/$stage/$name"
install -m644 README.md LICENSE "$dist/$stage/"

# Fixed owner, order and timestamps, so the same commit gives the same bytes.
tar -C "$dist" --owner=0 --group=0 --numeric-owner --sort=name \
    --mtime="@$(git log -1 --format=%ct)" -czf "$dist/$stage.tar.gz" "$stage"

echo "$dist/$stage.tar.gz"
