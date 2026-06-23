#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "GUI real-forward smoke currently requires macOS" >&2
  exit 2
fi

source "$(dirname "$0")/real_password_env.sh"
resolve_openterm_real_password_env

LOCAL_PORT="${OPENTERM_UI_REAL_FORWARD_PORT:-18022}"
STATUS_FILE="${OPENTERM_UI_REAL_FORWARD_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-forward.status}"
KNOWN_HOSTS_FILE="${OPENTERM_UI_REAL_FORWARD_KNOWN_HOSTS:-${TMPDIR:-/tmp}/openterm-ui-real-forward.known_hosts}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-ui-real-forward.XXXXXX.log")"
rm -f "$STATUS_FILE"
rm -f "$KNOWN_HOSTS_FILE"

cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE" "$LOG_FILE.banner"
}
trap cleanup EXIT

echo "OpenTerm GUI real-forward smoke"

cargo build -p openterm-app >"$LOG_FILE" 2>&1

OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_AUTO_FORWARD=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_FILE" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
OPENTERM_UI_SMOKE_FORWARD_PORT="$LOCAL_PORT" \
OPENTERM_UI_SMOKE_FORWARD_REMOTE_HOST="${OPENTERM_UI_REAL_FORWARD_REMOTE_HOST:-127.0.0.1}" \
OPENTERM_UI_SMOKE_FORWARD_REMOTE_PORT="${OPENTERM_UI_REAL_FORWARD_REMOTE_PORT:-22}" \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..160}; do
  if [[ -f "$STATUS_FILE" ]] && grep -q "state=forward_listening" "$STATUS_FILE"; then
    break
  fi
  if [[ -f "$STATUS_FILE" ]] && grep -q "state=ssh_error" "$STATUS_FILE"; then
    cat "$STATUS_FILE" >&2
    exit 1
  fi
  if ! kill -0 "$APP_PID" 2>/dev/null; then
    cat "$LOG_FILE" >&2
    [[ -f "$STATUS_FILE" ]] && cat "$STATUS_FILE" >&2
    exit 1
  fi
  sleep 0.25
done

if ! [[ -f "$STATUS_FILE" ]] || ! grep -q "state=forward_listening" "$STATUS_FILE"; then
  echo "timed out waiting for GUI forward listener" >&2
  [[ -f "$STATUS_FILE" ]] && cat "$STATUS_FILE" >&2
  exit 1
fi

nc -G 5 127.0.0.1 "$LOCAL_PORT" >"$LOG_FILE.banner" < /dev/null || true
grep -q "SSH-" "$LOG_FILE.banner"

echo "OpenTerm GUI real-forward smoke passed: $STATUS_FILE"
