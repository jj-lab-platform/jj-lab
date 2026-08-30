#!/usr/bin/env bash
# swift: A/B/C + SCM pull-through
set -uo pipefail
cd "$(dirname "$0")"
source ../lib.sh
source ../flag.sh
section "swift"
LOG="$WORK/swift.log"
: > "$LOG"
for p in a-pull b-push c-consume d-mutate e-upgrade f-republish; do
  [ -f "$p/run.sh" ] && bash "$p/run.sh" >> "$LOG" 2>&1
done
cat "$LOG"
PASS=$(grep -c 'PASS:' "$LOG" || true); FAIL=$(grep -c 'FAIL:' "$LOG" || true); SKIP=$(grep -c 'SKIP:' "$LOG" || true)
PASS=${PASS:-0}; FAIL=${FAIL:-0}; SKIP=${SKIP:-0}
echo "$PASS $FAIL $SKIP" > "$WORK/swift.result"
summary "swift"
