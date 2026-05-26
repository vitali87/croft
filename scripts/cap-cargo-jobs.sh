#!/usr/bin/env sh
# Regenerate .cargo/config.toml so cargo build/test/run uses at most 50% of
# this machine's logical cores and never locks the box. The generated file is
# gitignored, so every machine derives its own value. Run once per checkout.
set -eu

if command -v nproc >/dev/null 2>&1; then
  cores=$(nproc)
elif command -v sysctl >/dev/null 2>&1; then
  cores=$(sysctl -n hw.logicalcpu)
else
  cores=2
fi

half=$(( cores / 2 ))
[ "$half" -lt 1 ] && half=1

root=$(CDPATH= cd "$(dirname "$0")/.." && pwd)
mkdir -p "$root/.cargo"
cat > "$root/.cargo/config.toml" <<EOF
[build]
jobs = $half

[env]
RUST_TEST_THREADS = "$half"
EOF

echo "capped cargo at ${half}/${cores} logical cores -> $root/.cargo/config.toml"
