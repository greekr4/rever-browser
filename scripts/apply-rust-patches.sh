#!/bin/sh
# Recreate vendor/: crates.io sources plus our patches, which the workspace
# Cargo.toml's [patch.crates-io] points at. Run once after cloning, and again
# whenever a file under patches/ changes.
#   patches/<crate>@<version>.patch  →  vendor/<crate>
set -e
cd "$(dirname "$0")/.."
mkdir -p vendor
for patch in patches/*@*.patch; do
  spec=$(basename "$patch" .patch)
  name=${spec%@*}
  version=${spec#*@}
  rm -rf "vendor/$name" "vendor/$name-$version"
  curl -sSfL "https://static.crates.io/crates/$name/$name-$version.crate" | tar -xz -C vendor
  mv "vendor/$name-$version" "vendor/$name"
  patch -s -d "vendor/$name" -p1 < "$patch"
  echo "patched $name $version"
done
