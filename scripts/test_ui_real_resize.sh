#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "GUI real-resize smoke currently requires macOS" >&2
  exit 2
fi

source "$(dirname "$0")/real_password_env.sh"
resolve_openterm_real_password_env

STATUS_FILE="${OPENTERM_UI_REAL_RESIZE_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-resize.status}"
KNOWN_HOSTS_FILE="${OPENTERM_UI_REAL_RESIZE_KNOWN_HOSTS:-${TMPDIR:-/tmp}/openterm-ui-real-resize.known_hosts}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-ui-real-resize.XXXXXX.log")"
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

echo "OpenTerm GUI real-resize smoke"

cargo build -p openterm-app >"$LOG_FILE" 2>&1

OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_AUTO_CONNECT=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_AUTO_RESIZE_PROBE=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_FILE" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..220}; do
  if [[ -f "$STATUS_FILE" ]] \
    && grep -q "OPENTERM_SIZE_BEFORE=" "$STATUS_FILE" \
    && grep -q "OPENTERM_SIZE_AFTER=" "$STATUS_FILE"; then
    BEFORE="$(grep -Eo 'OPENTERM_SIZE_BEFORE=[0-9]+ [0-9]+' "$STATUS_FILE" | tail -1 | cut -d= -f2)"
    AFTER="$(grep -Eo 'OPENTERM_SIZE_AFTER=[0-9]+ [0-9]+' "$STATUS_FILE" | tail -1 | cut -d= -f2)"
    if [[ -n "$BEFORE" && -n "$AFTER" && "$BEFORE" != "$AFTER" ]]; then
      echo "OpenTerm GUI real-resize smoke passed: before=$BEFORE after=$AFTER"
      exit 0
    fi
  fi
  if [[ -f "$STATUS_FILE" ]] && grep -Eq "state=(ssh_error|shell_failed)" "$STATUS_FILE"; then
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

echo "timed out waiting for GUI resize proof" >&2
[[ -f "$STATUS_FILE" ]] && cat "$STATUS_FILE" >&2
exit 1
