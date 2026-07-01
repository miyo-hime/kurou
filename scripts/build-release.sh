#!/usr/bin/env bash
set -euo pipefail

# the crow's only blessed build. zig, not docker.
# every instinct says "aarch64 musl needs cross" - cross is a docker wrapper, docker
# is usually down. it already lured one crow toward docker and wasted a release. run this.
#
# ※ run this from a LINUX host (mother's archlinux WSL is set up for it: rustup + zig +
# cargo-zigbuild + the musl target). turso drags C build scripts (simsimd, zstd-sys) that
# sniff the *host* os and inject a windows-only advapi32 into the link when built from
# windows. on linux there's no host/target mismatch and it builds clean. don't build this
# from git-bash on windows - it compiles for ages then dies at the link step.

target="aarch64-unknown-linux-musl"
if [[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* || "$(uname -s)" == CYGWIN* ]]; then
    echo "refusing to build from a windows host - turso's C deps leak advapi32 into the musl link." >&2
    echo "run this from the archlinux WSL distro instead (see the header comment)." >&2
    exit 1
fi
cargo zigbuild --release --target "$target"

bin="target/$target/release/kurou"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

echo
echo "built: $bin"
file "$bin"
echo "size:  $(stat -c %s "$bin") bytes"
echo "asset: kurou-v${version}-aarch64-unknown-linux-musl"
