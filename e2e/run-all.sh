#!/usr/bin/env bash
# jjlab package-registry end-to-end suites (drives the real deployed service).
#
#   A (a-pull)    : official client pulls a PUBLIC package through pull-through
#   B (b-push)    : official client publishes a self-made package
#   C (c-consume) : a fresh project consumes the package pushed in B
#   D (d-mutate)  : deprecate/yank/retire/unlist/delete (or immutable 405)
#   E (e-upgrade) : publish a new version; assert latest/index move semantically
#   F (f-republish): same-version overwrite + delete-republish + name norm
#
# The target is the deployed jjlab service (in-process pkglab registry),
# NOT a local devserver. It is addressed via the cluster DNS name.
set -uo pipefail
CDIR="$(cd "$(dirname "$0")" && pwd)"
cd "$CDIR"
source ./lib.sh

# jjlab serves both the git REST surface and the package registry on one
# origin. We only check the registry health here (anonymous read of npm ping).
BASE="${BASE:-http://jj-lab.temp.svc.cluster.local}"
export BASE
curl -s --noproxy '*' --max-time 5 "$BASE/pkgs/npm/-/ping" >/dev/null 2>&1 \
  || { echo "registry not reachable at $BASE (deployed jjlab down?)"; exit 1; }

SUITES=(
  net-preflight
  npm
  pypi
  cargo
  go
  maven
  composer
  nuget
  rubygems
  hex
  pub
  swift
  conan
  helm
  generic
  oci
  auth
)
if [ $# -gt 0 ]; then SUITES=("$@"); fi

TOT_PASS=0; TOT_FAIL=0; TOT_SKIP=0; FAILED=()
for s in "${SUITES[@]}"; do
  f="$CDIR/$s/run.sh"
  [ -f "$f" ] || { echo "[$s] MISSING"; continue; }
  echo ""
  echo "════ $s ════"
  # Direct execution (no command substitution) so mix/gradle child processes
  # cannot hold the capture pipe open and hang the run (learned from ABC).
  bash "$f"
  res=$(cat "$WORK/$s.result" 2>/dev/null)
  p=$(echo "$res" | awk '{print $1}'); f2=$(echo "$res" | awk '{print $2}'); sk=$(echo "$res" | awk '{print $3}')
  p=${p:-0}; f2=${f2:-0}; sk=${sk:-0}
  echo "[$s] $p passed, $f2 failed, $sk skipped"
  TOT_PASS=$((TOT_PASS+p)); TOT_FAIL=$((TOT_FAIL+f2)); TOT_SKIP=$((TOT_SKIP+sk))
  if [ "$f2" != "0" ]; then FAILED+=("$s"); fi
done

echo ""
echo "=========================================="
echo "JJLAB REGISTRY E2E: $TOT_PASS passed, $TOT_FAIL failed, $TOT_SKIP skipped"
if [ ${#FAILED[@]} -gt 0 ]; then
  echo "failing: ${FAILED[*]}"
  exit 1
fi
echo "JJLAB REGISTRY E2E GREEN"
