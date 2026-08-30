#!/usr/bin/env bash
# C: consume the package pushed in B.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have composer || { skip "composer not installed"; exit 0; }
NAME=$(cat "$WORK/comp-name")
D=$(dir comp-c)
mkdir -p "$D/.composer"
echo '{"config":{"secure-http":false}}' > "$D/.composer/config.json"
cat > "$D/composer.json" <<CJ
{"require":{"e2e-test/$NAME":"1.0.0"},"repositories":[{"type":"composer","url":"$API/pkgs/composer"}]}
CJ
(cd "$D" && COMPOSER_HOME="$D/.composer" COMPOSER_CACHE_DIR="$D/.ccache" composer install -q --no-interaction --no-dev >/tmp/t.out 2>&1)
[ -d "$D/vendor/e2e-test/$NAME" ] && pass "C: composer install e2e-test/$NAME (just pushed)" || fail "C: composer install"
