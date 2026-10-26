#!/usr/bin/env bash
# Build the WebAssembly package for the 3D viewer into web3d/pkg.
#
# Note: we prepend ~/.cargo/bin so the *rustup* rustc (which has the
# wasm32-unknown-unknown target) wins over any other rustc on PATH — e.g. a
# Homebrew-installed rust, which won't have the wasm target.
#
# Heads up: Bevy's first wasm compile is SLOW (minutes) and the resulting wasm
# is multi-MB. That's expected.
set -euo pipefail
cd "$(dirname "$0")/.."

PATH="$HOME/.cargo/bin:$PATH" wasm-pack build viz --target web --out-dir ../web3d/pkg --release

# The scanned piece set is fetched by the wasm at runtime from ./assets/…
# relative to the page, so expose viz/assets there (a relative symlink; the
# http.server below follows it).
ln -sfn ../viz/assets web3d/assets

cat <<'EOF'

✅ Built web3d/pkg. Serve the demo over http (Web Serial needs a secure
context; localhost counts):

    cd web3d && python3 -m http.server 8000

Then open http://localhost:8000 in Chrome or Edge, click "Connect board",
and pick the DGT serial port. Left-drag to orbit, scroll to zoom.
EOF
