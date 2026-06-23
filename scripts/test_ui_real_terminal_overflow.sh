#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "GUI terminal-overflow smoke currently requires macOS" >&2
  exit 2
fi

source "$(dirname "$0")/real_password_env.sh"
resolve_openterm_real_password_env

STATUS_FILE="${OPENTERM_UI_REAL_OVERFLOW_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-overflow.status}"
KNOWN_HOSTS_FILE="${OPENTERM_UI_REAL_OVERFLOW_KNOWN_HOSTS:-${TMPDIR:-/tmp}/openterm-ui-real-overflow.known_hosts}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-ui-real-overflow.XXXXXX.log")"
rm -f "$STATUS_FILE" "$KNOWN_HOSTS_FILE"

cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE"
}
trap cleanup EXIT

echo "OpenTerm GUI terminal-overflow smoke"

cargo build -p openterm-app >"$LOG_FILE" 2>&1

OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_AUTO_CONNECT=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_OVERFLOW_PROBE=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_FILE" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..220}; do
  if [[ -f "$STATUS_FILE" ]] \
    && grep -q "OPENTERM_OVERFLOW_DONE" "$STATUS_FILE" \
    && grep -q "terminal_overflow=true" "$STATUS_FILE" \
    && grep -q "panel=Terminal" "$STATUS_FILE"; then
    ROWS="$(grep -Eo 'terminal_rows=[0-9]+' "$STATUS_FILE" | tail -1 | cut -d= -f2)"
    LINES="$(grep -Eo 'output_lines=[0-9]+' "$STATUS_FILE" | cut -d= -f2 | sort -n | tail -1)"
    if [[ -n "$ROWS" && -n "$LINES" && "$LINES" -gt "$ROWS" ]]; then
      echo "OpenTerm GUI terminal-overflow smoke passed: rows=$ROWS output_lines=$LINES"
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

echo "timed out waiting for terminal overflow proof" >&2
[[ -f "$STATUS_FILE" ]] && cat "$STATUS_FILE" >&2
exit 1
