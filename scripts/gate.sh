#!/usr/bin/env bash
# Standing gate runner (HANDOFF §13). Each gate's exit status is captured
# directly from the cargo invocation, never from a downstream pipe stage.
# Usage: scripts/gate.sh [fast|full]
set -u
MODE="${1:-fast}"
FAIL=0
LOG="$(mktemp -d)/gate"

run() {
  local name="$1"; shift
  "$@" >"$LOG.$name" 2>&1
  local s=$?
  if [ $s -ne 0 ]; then
    FAIL=1
    echo "GATE $name: FAIL (exit $s)"
    tail -30 "$LOG.$name" | sed "s/^/  [$name] /"
  else
    echo "GATE $name: ok"
  fi
  return 0
}

run build   cargo build --workspace -q
run test    cargo test --workspace -q
run floor   cargo run -q -p pon-conformance -- --suite cpython --check-floor
run aot     cargo run -q -p pon-conformance -- --mode aot --suite cpython-aot-subset --check-floor

if [ "$MODE" = "full" ]; then
  run ft       cargo test --workspace --features free-threading -q
  run ftstress cargo run -q -p pon-conformance --features free-threading -- --suite ft-stress
  run bench    cargo run -q -p pon-conformance -- --bench
fi

echo "GATE-SUMMARY: $([ $FAIL -eq 0 ] && echo ALL-GREEN || echo RED)"
exit $FAIL
