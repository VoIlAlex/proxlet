#!/usr/bin/env bash
# Starts Pebble (Let's Encrypt's test ACME server) and pebble-challtestsrv
# (its DNS, answering 127.0.0.1 for every name) for CI.
#
#   pebble.sh HTTP_PORT
#
# Pebble validates HTTP-01 challenges on 127.0.0.1:HTTP_PORT. Afterwards:
#   https://localhost:14000/dir    the ACME directory
#   /tmp/pebble/minica.pem         trusts Pebble's own HTTPS (PROXLET_ACME_ROOTS)
#   /tmp/pebble/root.pem           the root that signs issued certificates
set -euo pipefail
HTTP_PORT="$1"
VERSION=v2.10.3
DIR=/tmp/pebble
mkdir -p "$DIR/bin"
for tool in pebble pebble-challtestsrv; do
  curl -fsSL "https://github.com/letsencrypt/pebble/releases/download/$VERSION/$tool-linux-amd64.tar.gz" \
    | tar xz -C "$DIR/bin"
  install -m 0755 "$DIR/bin/$tool-linux-amd64/linux/amd64/$tool" "$DIR/$tool"
done
git clone -q --depth 1 --branch "$VERSION" https://github.com/letsencrypt/pebble "$DIR/src"
cp "$DIR/src/test/certs/pebble.minica.pem" "$DIR/minica.pem"
jq --arg c "$DIR/src/test/certs/localhost/cert.pem" \
   --arg k "$DIR/src/test/certs/localhost/key.pem" \
   --argjson p "$HTTP_PORT" \
   '.pebble.certificate = $c | .pebble.privateKey = $k | .pebble.httpPort = $p' \
   "$DIR/src/test/config/pebble-config.json" > "$DIR/config.json"

"$DIR/pebble-challtestsrv" -defaultIPv4 127.0.0.1 -defaultIPv6 "" \
  -dnsserver :8053 -http01 "" -https01 "" -tlsalpn01 "" -doh "" > "$DIR/challtestsrv.log" 2>&1 &
PEBBLE_VA_NOSLEEP=1 PEBBLE_WFE_NONCEREJECT=0 \
  "$DIR/pebble" -config "$DIR/config.json" -dnsserver 127.0.0.1:8053 > "$DIR/pebble.log" 2>&1 &

for _ in $(seq 50); do
  curl -fsS --cacert "$DIR/minica.pem" https://localhost:14000/dir > /dev/null 2>&1 && break
  sleep 0.2
done
curl -fsS --cacert "$DIR/minica.pem" https://localhost:14000/dir > /dev/null
curl -fsS --cacert "$DIR/minica.pem" https://localhost:15000/roots/0 > "$DIR/root.pem"
echo "Pebble is up; validating HTTP-01 on port $HTTP_PORT"
