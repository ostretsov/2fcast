#!/bin/bash
# Sets the version of both crates (and so of the .deb): scripts/bump-version.sh 0.3.1
set -euo pipefail
cd "$(dirname "$0")/.."

new=${1:?usage: $0 <version>, e.g. 0.3.1}
[[ $new =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || { echo "Not a version: $new" >&2; exit 1; }
old=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/app/Cargo.toml | head -1)
[ "$old" = "$new" ] && { echo "Version is already $new"; exit 0; }

# The first `version =` line is the [package] one.
for f in crates/app/Cargo.toml crates/core/Cargo.toml; do
	sed -i "0,/^version = \".*\"/s//version = \"$new\"/" "$f"
done
sed -i "s/^\(twofcast-core = { version = \)\"[^\"]*\"/\1\"$new\"/" crates/app/Cargo.toml
# The workspace crates' entries in Cargo.lock.
sed -i "/^name = \"twofcast\(-core\)\?\"$/{n;s/^version = \".*\"/version = \"$new\"/}" Cargo.lock
sed -i "s/2fcast_${old//./\\.}_amd64/2fcast_${new}_amd64/g" README.md

echo "Version $old -> $new"
