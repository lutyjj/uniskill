#!/bin/sh
set -eu

case "${GITHUB_REF:-}" in
    refs/tags/v*) ;;
    *) echo 'release requires an annotated version tag' >&2; exit 1 ;;
esac

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
tag=${GITHUB_REF#refs/tags/}
if [ -z "$version" ] || [ "$tag" != "v$version" ]; then
    echo 'release tag does not match the package version' >&2
    exit 1
fi
if [ "$(git cat-file -t "$GITHUB_REF")" != tag ]; then
    echo 'release tag must be annotated' >&2
    exit 1
fi
commit=$(git rev-parse "$GITHUB_REF^{commit}")
if [ "$commit" != "$(git rev-parse HEAD)" ]; then
    echo 'release tag does not identify the checked-out commit' >&2
    exit 1
fi
printf 'version=%s\ntag=%s\ncommit=%s\n' "$version" "$tag" "$commit" >> "$GITHUB_OUTPUT"
