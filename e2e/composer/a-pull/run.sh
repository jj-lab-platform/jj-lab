#!/usr/bin/env bash
# A: pull a public package.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have composer || { skip "composer not installed"; exit 0; }
D=$(dir comp-a)
mkdir -p "$D/.composer"
echo '{"config":{"secure-http":false}}' > "$D/.composer/config.json"
cat > "$D/composer.json" <<CJ
{"require":{"psr/log":"1.1.4"},"repositories":[{"type":"composer","url":"$API/pkgs/composer"}]}
CJ
(cd "$D" && COMPOSER_HOME="$D/.composer" COMPOSER_CACHE_DIR="$D/.ccache" composer install -q --no-interaction --no-dev >/tmp/t.out 2>&1)
[ -d "$D/vendor/psr/log" ] && pass "A: composer install psr/log via pull-through" || fail "A: composer install psr/log"
