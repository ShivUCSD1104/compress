#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
python3 scripts/package-homebrew.py
tap_dir=$(brew --repository local/compress)
if [ ! -d "$tap_dir/Formula" ]; then
  brew tap-new local/compress
fi
tap_dir=$(brew --repository local/compress)
cp Formula/compress-cli.rb "$tap_dir/Formula/compress-cli.rb"
if brew list --versions local/compress/compress-cli >/dev/null 2>&1; then
  brew reinstall local/compress/compress-cli
else
  brew install local/compress/compress-cli
fi
brew test local/compress/compress-cli
