#!/bin/bash
set -e

# bump version
vim echo-operator_core/Cargo.toml

# update lock file
cargo update -p echo-operator_core

make update-changelog

git add .
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' echo-operator_core/Cargo.toml | head -n1)
git commit -m "release: Version $VERSION"

echo "After merging the PR, tag and release are automatically done"
