#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "GUI real-SFTP smoke currently requires macOS" >&2
  exit 2
fi

source "$(dirname "$0")/real_password_env.sh"
resolve_openterm_real_password_env

STATUS_FILE="${OPENTERM_UI_REAL_SFTP_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-sftp.status}"
KNOWN_HOSTS_FILE="${OPENTERM_UI_REAL_SFTP_KNOWN_HOSTS:-${TMPDIR:-/tmp}/openterm-ui-real-sftp.known_hosts}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-ui-real-sftp.XXXXXX.log")"
rm -f "$STATUS_FILE"
rm -f "$KNOWN_HOSTS_FILE"

cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE"
}
trap cleanup EXIT

echo "OpenTerm GUI real-SFTP smoke"

cargo build -p openterm-app >"$LOG_FILE" 2>&1

OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_AUTO_SFTP=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_FILE" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..160}; do
  if [[ -f "$STATUS_FILE" ]] && grep -q "state=sftp_loaded" "$STATUS_FILE"; then
    echo "OpenTerm GUI real-SFTP smoke passed: $STATUS_FILE"
    exit 0
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

echo "timed out waiting for GUI SFTP listing" >&2
[[ -f "$STATUS_FILE" ]] && cat "$STATUS_FILE" >&2
exit 1
