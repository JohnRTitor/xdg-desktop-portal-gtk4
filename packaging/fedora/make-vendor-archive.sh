#!/bin/sh
# Build the Source1 that packaging/fedora/xdg-desktop-portal-gtk4.spec expects.
#
# Usage:
#   packaging/fedora/make-vendor-archive.sh [OUTPUT]

set -eu

srcdir=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$srcdir"

version=$(sed -n '/^\[package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
if [ -z "$version" ]; then
    echo "make-vendor-archive: no [package] version in Cargo.toml" >&2
    exit 1
fi

output=${1:-"$srcdir/packaging/fedora/xdg-desktop-portal-gtk4-$version-vendor.tar.xz"}

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT INT HUP TERM

cargo vendor --locked --versioned-dirs "$workdir/vendor" >/dev/null

cat >"$workdir/vendor-config.toml" <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "@VENDOR_DIR@"
EOF

mkdir -p "$(dirname -- "$output")"
tar --create --xz --file="$output" \
    --directory="$workdir" \
    --sort=name --owner=0 --group=0 --numeric-owner \
    --mtime="@${SOURCE_DATE_EPOCH:-0}" \
    vendor vendor-config.toml

echo "wrote $output"
