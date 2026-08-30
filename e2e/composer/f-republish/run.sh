#!/usr/bin/env bash
# F: republish — same vendor/package/version zip with different bytes must be
# served back from /dist (replace, not append).
set -uo pipefail
cd "$(dirname "$0")"
source ../../lib.sh
NAME=$(cat "$WORK/comp-name")
D=$(dir comp-f)

# v1 then v2 zips (same package name/version).
for N in 1 2; do
  mkdir -p "$D/v$N/src"
  cat > "$D/v$N/composer.json" <<CJ
{"name":"e2e-test/$NAME","version":"1.0.0","description":"v$N","require":{"php":">=7.4"},"autoload":{"psr-4":{"E2e\\\\Lib\\\\":"src/"}}}
CJ
  echo "<?php namespace E2e\\Lib; class Lib { public static function v(){ return 'v$N'; } }" > "$D/v$N/src/Lib.php"
  python3 -c "import zipfile; z=zipfile.ZipFile('$D/v$N/pkg.zip','w'); z.write('$D/v$N/composer.json','composer.json'); z.write('$D/v$N/src/Lib.php','src/Lib.php'); z.close()"
done

curl_api -s -o /dev/null -X PUT "$API/pkgs/composer/api/packages?name=e2e-test/$NAME&version=1.0.0" --data-binary "@$D/v1/pkg.zip"
code=$(curl_api -s -o /dev/null -w '%{http_code}' -X PUT "$API/pkgs/composer/api/packages?name=e2e-test/$NAME&version=1.0.0" --data-binary "@$D/v2/pkg.zip")
assert_eq "F: composer republish" 201 "$code"

# dist URL zip content must be v2 ("v2" inside Lib.php). Binary: fetch to a
# file, avoid command substitution (bash strips null bytes).
curl_api -s "$API/pkgs/composer/dist/e2e-test/$NAME/1.0.0/ref" -o "$D/dl.zip"
python3 -c "import zipfile; z=zipfile.ZipFile('$D/dl.zip'); print(z.read('src/Lib.php').decode())" | grep -q "v2" \
  && pass "F: dist zip overwritten" || fail "F: dist zip overwritten"