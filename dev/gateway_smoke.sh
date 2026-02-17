#!/usr/bin/env bash
set -euo pipefail

HOST="${HOST:-127.0.0.1}"
PORT="${PORT:-3000}"
CONTAINER="${CONTAINER:-zeroclaw-dev}"
PAIR_CODE="${PAIR_CODE:-}"
MESSAGE="${MESSAGE:-Reply with: LLM path works.}"
AUTO_PAIR_CODE=0

usage() {
  cat <<'EOF'
ZeroClaw gateway + LLM smoke test

Usage:
  ./dev/gateway_smoke.sh [options]

Options:
  --host <host>            Gateway host (default: 127.0.0.1)
  --port <port>            Gateway port (default: 3000)
  --container <name>       Dev container name (default: zeroclaw-dev)
  --pair-code <6digits>    Pairing code (if omitted, parsed from container logs)
  --message <text>         Webhook message payload
  -h, --help               Show this help
EOF
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "error: required command not found: $1" >&2
    exit 1
  fi
}

json_escape() {
  local s="$1"
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\n'/\\n}
  s=${s//$'\r'/\\r}
  printf '%s' "$s"
}

extract_token() {
  local body="$1"
  printf '%s' "$body" | sed -n 's/.*"token"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p'
}

get_http_body() {
  printf '%s' "$1" | sed '$d'
}

get_http_code() {
  printf '%s' "$1" | tail -n1
}

resolve_pair_code() {
  local container="$1"
  local started_at=""
  local logs=""
  local code=""

  started_at="$(docker inspect -f '{{.State.StartedAt}}' "$container" 2>/dev/null || true)"
  if [ -n "$started_at" ]; then
    logs="$(docker logs --since "$started_at" "$container" 2>&1 || true)"
  fi

  if [ -z "$logs" ]; then
    logs="$(docker logs "$container" 2>&1 || true)"
  fi

  code="$(
    printf '%s\n' "$logs" \
      | sed -n 's/.*X-Pairing-Code: \([0-9][0-9][0-9][0-9][0-9][0-9]\).*/\1/p' \
      | tail -n1
  )"
  printf '%s' "$code"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --host)
      HOST="$2"
      shift 2
      ;;
    --port)
      PORT="$2"
      shift 2
      ;;
    --container)
      CONTAINER="$2"
      shift 2
      ;;
    --pair-code)
      PAIR_CODE="$2"
      shift 2
      ;;
    --message)
      MESSAGE="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

require_cmd docker
require_cmd curl

BASE_URL="http://${HOST}:${PORT}"
HEALTH_URL="${BASE_URL}/health"
PAIR_URL="${BASE_URL}/pair"
WEBHOOK_URL="${BASE_URL}/webhook"

echo "Checking gateway health at ${HEALTH_URL} ..."
if ! curl -fsS "$HEALTH_URL" >/dev/null; then
  echo "error: gateway is not reachable at ${BASE_URL}" >&2
  echo "hint: start it with ./dev/cli.sh up" >&2
  exit 1
fi
echo "Health check: OK"

if [ -z "$PAIR_CODE" ]; then
  AUTO_PAIR_CODE=1
  echo "Reading latest pairing code from container logs (${CONTAINER}) ..."
  PAIR_CODE="$(resolve_pair_code "$CONTAINER")"
fi

if ! printf '%s' "$PAIR_CODE" | grep -Eq '^[0-9]{6}$'; then
  echo "error: could not resolve a valid 6-digit pairing code" >&2
  echo "hint: check logs with ./dev/cli.sh logs and rerun (or pass --pair-code)" >&2
  exit 1
fi
echo "Using pairing code: ${PAIR_CODE}"

PAIR_RESPONSE="$(
  curl -sS -X POST "$PAIR_URL" \
    -H "X-Pairing-Code: ${PAIR_CODE}" \
    -H "Content-Type: application/json" \
    -w $'\n%{http_code}'
)"
PAIR_BODY="$(get_http_body "$PAIR_RESPONSE")"
PAIR_STATUS="$(get_http_code "$PAIR_RESPONSE")"

if [ "$AUTO_PAIR_CODE" -eq 1 ] && [ "$PAIR_STATUS" = "403" ] && printf '%s' "$PAIR_BODY" | grep -q 'Invalid pairing code'; then
  NEW_PAIR_CODE="$(resolve_pair_code "$CONTAINER")"
  if [ -n "$NEW_PAIR_CODE" ] && [ "$NEW_PAIR_CODE" != "$PAIR_CODE" ]; then
    echo "Detected pairing-code mismatch. Retrying with latest code: ${NEW_PAIR_CODE}"
    PAIR_CODE="$NEW_PAIR_CODE"
    PAIR_RESPONSE="$(
      curl -sS -X POST "$PAIR_URL" \
        -H "X-Pairing-Code: ${PAIR_CODE}" \
        -H "Content-Type: application/json" \
        -w $'\n%{http_code}'
    )"
    PAIR_BODY="$(get_http_body "$PAIR_RESPONSE")"
    PAIR_STATUS="$(get_http_code "$PAIR_RESPONSE")"
  fi
fi

if [ "${PAIR_STATUS}" -lt 200 ] || [ "${PAIR_STATUS}" -ge 300 ]; then
  echo "error: pair request failed (HTTP ${PAIR_STATUS})" >&2
  echo "$PAIR_BODY" >&2
  if [ "$PAIR_STATUS" = "403" ] && printf '%s' "$PAIR_BODY" | grep -q 'Invalid pairing code'; then
    echo "hint: pairing code may be stale; restart gateway and rerun: ./dev/cli.sh down && ./dev/cli.sh up && ./dev/cli.sh smoke" >&2
  fi
  exit 1
fi

TOKEN="$(extract_token "$PAIR_BODY")"
if [ -z "$TOKEN" ]; then
  echo "error: pairing succeeded but token was not found in response" >&2
  echo "$PAIR_BODY" >&2
  exit 1
fi
echo "Pairing: OK (token length: ${#TOKEN})"

MESSAGE_JSON="{\"message\":\"$(json_escape "$MESSAGE")\"}"
WEBHOOK_RESPONSE="$(
  curl -sS -X POST "$WEBHOOK_URL" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d "$MESSAGE_JSON" \
    -w $'\n%{http_code}'
)"
WEBHOOK_BODY="$(get_http_body "$WEBHOOK_RESPONSE")"
WEBHOOK_STATUS="$(get_http_code "$WEBHOOK_RESPONSE")"

if [ "${WEBHOOK_STATUS}" -lt 200 ] || [ "${WEBHOOK_STATUS}" -ge 300 ]; then
  echo "error: webhook request failed (HTTP ${WEBHOOK_STATUS})" >&2
  echo "$WEBHOOK_BODY" >&2
  exit 1
fi

echo "Webhook: OK (HTTP ${WEBHOOK_STATUS})"
echo
echo "Response:"
if command -v jq >/dev/null 2>&1; then
  printf '%s\n' "$WEBHOOK_BODY" | jq .
else
  printf '%s\n' "$WEBHOOK_BODY"
fi
