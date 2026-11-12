#!/bin/sh
set -eu
cd "$(dirname "$0")"
cargo build --release --target wasm32-wasip2
echo "built target/wasm32-wasip2/release/haunt_plugin.wasm; install with: haunt plugin install ."
