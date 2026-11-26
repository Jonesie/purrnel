#!/usr/bin/env bash
# Start a built Purrnel image and check it really works: serves the page, loads the speech models,
# lets its own page open a websocket and refuses any other website. Used by CI and the release
# workflow, and handy locally:  scripts/smoke-test.sh [image] [port]
set -euo pipefail

image="${1:-purrnel:latest}"
port="${2:-7990}"
name="purrnel-smoke-$$"

cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    echo "--- container log ---" >&2
    docker logs "$name" 2>&1 | grep -v 'microphone problem' | tail -40 >&2 || true
  fi
  docker rm -f "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

fail() { echo "smoke test FAILED: $*" >&2; exit 1; }

# No sound server here, so the cat can neither hear nor speak out loud; everything else still runs.
docker run -d --name "$name" -p "127.0.0.1:$port:$port" -e "PURRNEL_PORT=$port" "$image" >/dev/null

echo "waiting for the page..."
for _ in $(seq 1 60); do
  curl -fs -o /dev/null "http://127.0.0.1:$port/" && break
  sleep 1
done
code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/")
[ "$code" = 200 ] || fail "GET / returned $code"

echo "waiting for the speech models..."
for _ in $(seq 1 120); do
  docker logs "$name" 2>&1 | grep -q 'purrnel: ready' && break
  sleep 1
done
docker logs "$name" 2>&1 | grep -q 'purrnel: ready (voice: true, ears: true)' \
  || fail "voice or speech recognition did not start"

# A websocket upgrade from another website must be refused; from the page's own address it works.
ws() {
  curl -s --max-time 3 -o /dev/null -w '%{http_code}' \
    -H "Origin: $1" -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
    -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
    "http://127.0.0.1:$port/ws" || true
}
[ "$(ws https://evil.example)" = 403 ] || fail "a foreign origin was not refused"
[ "$(ws "http://localhost:$port")" = 101 ] || fail "the page's own origin could not open a websocket"

echo "smoke test passed"
