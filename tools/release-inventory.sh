#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

mkdir -p target

echo "==> Writing Rust dependency inventory"
cargo metadata --format-version=1 > target/cargo-metadata.json

echo "==> Writing npm dependency inventory"
npm --prefix ui ls --json > target/npm-tree.json

echo "==> Release inventory written"
echo "    target/cargo-metadata.json"
echo "    target/npm-tree.json"
