#!/usr/bin/env bash
# E: upgrade — push 1.0.1, assert p2 metadata lists both versions.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have composer || { skip "composer not installed"; exit 0; }
NAME=$(cat "$WORK/comp-name")
D=$(dir comp-e)
cat > "$D/composer.json" <<CJ
{"name":"e2e-test/$NAME","version":"1.0.1","require":{"php":">=7.4"},"autoload":{"psr-4":{"E2e\\\\Lib\\\\":"src/"}}}
CJ
mkdir -p "$D/src"
echo '<?php namespace E2e\Lib; class Lib { public static function v(){ return "hello"; } }' > "$D/src/Lib.php"
python3 -c "import zipfile; z=zipfile.ZipFile('$D/pkg.zip','w'); z.write('$D/composer.json','composer.json'); z.write('$D/src/Lib.php','src/Lib.php'); z.close()"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/composer/api/packages?name=e2e-test/$NAME&version=1.0.1" --data-binary "@$D/pkg.zip")
assert_eq "E: composer push 1.0.1" 201 "$code"
out=$(curl_api "$API/pkgs/composer/p2/e2e-test/$NAME.json")
echo "$out" | grep -q '"version":"1.0.1"' && pass "E: p2 lists 1.0.1" || fail "E: p2 lists 1.0.1"
