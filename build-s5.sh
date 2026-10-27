#!/bin/sh
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
cargo zigbuild --release --locked --target armv7-unknown-linux-musleabihf
