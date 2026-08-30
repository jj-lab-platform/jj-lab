#!/usr/bin/env bash
# B: publish a self-made package.
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
have composer || { skip "composer not installed"; exit 0; }
NAME="e2e-comp-$RUN_ID"
echo "$NAME" > "$WORK/comp-name"
D=$(dir comp-b)
cat > "$D/composer.json" <<CJ
{"name":"e2e-test/$NAME","version":"1.0.0","require":{"php":">=7.4"},"autoload":{"psr-4":{"E2e\\\\Lib\\\\":"src/"}}}
CJ
mkdir -p "$D/src"
echo '<?php namespace E2e\Lib; class Lib { public static function v(){ return "hello"; } }' > "$D/src/Lib.php"
python3 -c "import zipfile; z=zipfile.ZipFile('$D/pkg.zip','w'); z.write('$D/composer.json','composer.json'); z.write('$D/src/Lib.php','src/Lib.php'); z.close()"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/composer/api/packages?name=e2e-test/$NAME&version=1.0.0" --data-binary "@$D/pkg.zip")
assert_eq "B: composer push e2e-test/$NAME" 201 "$code"
