#!/usr/bin/env bash
# Build the WebAssembly package for the Web Serial demo into web/pkg.
#
# Note: we prepend ~/.cargo/bin so the *rustup* rustc (which has the
# wasm32-unknown-unknown target) wins over any other rustc on PATH — e.g. a
# Homebrew-installed rust, which won't have the wasm target.
set -euo pipefail
cd "$(dirname "$0")/.."

PATH="$HOME/.cargo/bin:$PATH" wasm-pack build wasm --target web --out-dir ../web/pkg --release

cat <<'EOF'

✅ Built web/pkg. Serve the demo over http (Web Serial needs a secure context;
localhost counts):

    cd web && python3 -m http.server 8000

Then open http://localhost:8000 in Chrome or Edge and click "Connect board".
EOF
