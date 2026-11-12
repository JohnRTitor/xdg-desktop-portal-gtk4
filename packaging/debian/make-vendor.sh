#!/bin/sh
# Build the vendored Cargo dependencies that debian/rules expects.
#
# Usage:
#   cp -r packaging/debian debian
#   debian/make-vendor.sh [OUTPUT]

set -eu

dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
while [ ! -f "$dir/Cargo.toml" ]; do
    dir=$(dirname -- "$dir")
    if [ "$dir" = / ]; then
        echo "make-vendor.sh: no Cargo.toml above $0" >&2
        exit 1
    fi
done
srcdir=$dir
cd "$srcdir"

if [ ! -d debian ]; then
    echo "make-vendor.sh: $srcdir/debian does not exist." >&2
    echo "Copy the packaging into place first: cp -r packaging/debian debian" >&2
    exit 1
fi

output=${1:-"$srcdir/debian/vendor.tar.xz"}

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
