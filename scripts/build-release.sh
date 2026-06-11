#!/usr/bin/env bash
set -euo pipefail

# the crow's only blessed build. zig, not docker.
# every instinct says "aarch64 musl needs cross" - cross is a docker wrapper, docker
# is usually down, and we're rustls-only so zig has no C deps to wrestle. it already
# lured one crow toward docker and wasted a release. don't be that crow. run this.

target="aarch64-unknown-linux-musl"
cargo zigbuild --release --target "$target"

bin="target/$target/release/kurou"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

echo
echo "built: $bin"
file "$bin"
echo "size:  $(stat -c %s "$bin") bytes"
echo "asset: kurou-v${version}-aarch64-unknown-linux-musl"
