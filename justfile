# hop-hop runs `just deploy` on a deploy-branch push; hands run it bare
# when the daemon's asleep. same recipe, same door, no drift.

target := "aarch64-unknown-linux-musl"

build:
    cargo zigbuild --release --target {{target}}

deploy: build
    #!/usr/bin/env bash
    set -euo pipefail
    tag="v$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
    bin="${CARGO_TARGET_DIR:-target}/{{target}}/release/kurou"
    ssh kurobox "mkdir -p services/kurou/staging/$tag"
    rsync -a "$bin" "kurobox:services/kurou/staging/$tag/"
    ssh kurobox "kami update kurou --staged"
