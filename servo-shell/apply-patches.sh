#!/bin/sh
# Recreate vendor/servo-net: the crates.io source of servo-net 0.8.0 plus the
# network tap patch that Cargo.toml's [patch.crates-io] points at.
set -e
cd "$(dirname "$0")"
rm -rf vendor/servo-net && mkdir -p vendor
curl -sSfL https://static.crates.io/crates/servo-net/servo-net-0.8.0.crate | tar -xz -C vendor
mv vendor/servo-net-0.8.0 vendor/servo-net
patch -s -d vendor/servo-net -p1 < patches/servo-net-0.8.0-network-tap.patch
