#!/bin/bash
# Check serde fingerprints and rlibs in a fresh fixture build.
# Usage: ./tests/check_serde_fps.sh

tmp=$(mktemp -d /dlds/tmp/serde_chk.XXXXXX)
trap "rm -rf $tmp" EXIT

CARGO_TARGET_DIR="$tmp/target" cargo build -p cargo-gc-fixture \
    --manifest-path fixture/Cargo.toml 2>/dev/null

echo "=== serde fingerprints ==="
ls "$tmp"/target/debug/.fingerprint/ | grep "^serde-"

echo "=== serde rlibs ==="
ls "$tmp"/target/debug/deps/ | grep "^libserde-.*\.rlib"

echo "=== target/profile per fingerprint ==="
for d in "$tmp"/target/debug/.fingerprint/serde-*/
do name=$(basename "$d")
   json=$(cat "$d"/*.json 2>/dev/null)
   if test -n "$json"
   then target=$(echo "$json" | python3 -c "import sys,json; print(json.load(sys.stdin)['target'])")
        profile=$(echo "$json" | python3 -c "import sys,json; print(json.load(sys.stdin)['profile'])")
        echo "  $name  target=$target  profile=$profile"
   else echo "  $name  (no json)"
   fi
done
