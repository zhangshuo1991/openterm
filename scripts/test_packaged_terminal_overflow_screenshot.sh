#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Packaged terminal-overflow screenshot smoke currently requires macOS" >&2
  exit 2
fi

source "$(dirname "$0")/real_password_env.sh"
resolve_openterm_real_password_env

APP_BIN="${OPENTERM_PACKAGED_APP_BIN:-dist/OpenTerm.app/Contents/MacOS/OpenTerm}"
if [[ ! -x "$APP_BIN" ]]; then
  echo "missing packaged app binary: $APP_BIN" >&2
  echo "run ./scripts/package_macos.sh first" >&2
  exit 1
fi

STATUS_FILE="${OPENTERM_PACKAGED_OVERFLOW_STATUS:-${TMPDIR:-/tmp}/openterm-packaged-overflow.status}"
KNOWN_HOSTS_FILE="${OPENTERM_PACKAGED_OVERFLOW_KNOWN_HOSTS:-${TMPDIR:-/tmp}/openterm-packaged-overflow.known_hosts}"
DB_FILE="${OPENTERM_PACKAGED_OVERFLOW_DB:-${TMPDIR:-/tmp}/openterm-packaged-overflow.redb}"
SCREENSHOT="${OPENTERM_PACKAGED_OVERFLOW_SCREENSHOT:-dist/openterm-packaged-overflow.png}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-packaged-overflow.XXXXXX.log")"
MIN_BYTES="${OPENTERM_PACKAGED_OVERFLOW_MIN_BYTES:-100000}"
rm -f "$STATUS_FILE" "$KNOWN_HOSTS_FILE" "$DB_FILE" "$SCREENSHOT"

cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE"
}
trap cleanup EXIT

echo "OpenTerm packaged terminal-overflow screenshot smoke"

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_AUTO_CONNECT=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_OVERFLOW_PROBE=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_FILE" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
  "$APP_BIN" >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..220}; do
  if [[ -f "$STATUS_FILE" ]] \
    && grep -q "OPENTERM_OVERFLOW_DONE" "$STATUS_FILE" \
    && grep -q "terminal_overflow=true" "$STATUS_FILE" \
    && grep -q "panel=Terminal" "$STATUS_FILE"; then
    ROWS="$(grep -Eo 'terminal_rows=[0-9]+' "$STATUS_FILE" | tail -1 | cut -d= -f2)"
    LINES="$(grep -Eo 'output_lines=[0-9]+' "$STATUS_FILE" | cut -d= -f2 | sort -n | tail -1)"
    if [[ -n "$ROWS" && -n "$LINES" && "$LINES" -gt "$ROWS" ]]; then
      break
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

if [[ -z "${ROWS:-}" || -z "${LINES:-}" || "$LINES" -le "$ROWS" ]]; then
  echo "timed out waiting for packaged overflow proof" >&2
  [[ -f "$STATUS_FILE" ]] && cat "$STATUS_FILE" >&2
  exit 1
fi

if command -v osascript >/dev/null 2>&1; then
  osascript >/dev/null 2>&1 <<APPLESCRIPT || true
tell application "System Events"
  set frontmost of first process whose unix id is $APP_PID to true
end tell
APPLESCRIPT
  sleep 0.5
fi

screencapture -x "$SCREENSHOT"
BYTES="$(wc -c <"$SCREENSHOT" | tr -d '[:space:]')"
if [[ "$BYTES" -lt "$MIN_BYTES" ]]; then
  echo "screenshot too small: $BYTES bytes at $SCREENSHOT" >&2
  cat "$STATUS_FILE" >&2
  exit 1
fi

echo "OpenTerm packaged terminal-overflow screenshot smoke passed: rows=$ROWS output_lines=$LINES screenshot=$SCREENSHOT bytes=$BYTES"
