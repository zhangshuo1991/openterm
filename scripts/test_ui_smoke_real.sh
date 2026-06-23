#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "GUI smoke currently requires macOS screencapture" >&2
  exit 2
fi

SCREENSHOT="${OPENTERM_UI_SMOKE_SCREENSHOT:-${TMPDIR:-/tmp}/openterm-ui-smoke.png}"
MIN_BYTES="${OPENTERM_UI_SMOKE_MIN_BYTES:-250000}"
PREFILL_TEST_SERVER="${OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER:-0}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-ui-smoke.XXXXXX.log")"
STATUS_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-ui-smoke-status.XXXXXX.log")"

cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE" "$STATUS_FILE"
}
trap cleanup EXIT

echo "OpenTerm GUI smoke"

cargo build -p openterm-app >"$LOG_FILE" 2>&1

if [[ "$PREFILL_TEST_SERVER" == "1" ]]; then
  OPENTERM_UI_SMOKE_STATUS="$STATUS_FILE" \
  OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
  OPENTERM_UI_SMOKE_OPEN_SFTP="${OPENTERM_UI_SMOKE_OPEN_SFTP:-}" \
    target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
else
  OPENTERM_UI_SMOKE_STATUS="$STATUS_FILE" \
  OPENTERM_UI_SMOKE_OPEN_SFTP="${OPENTERM_UI_SMOKE_OPEN_SFTP:-}" \
    target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
fi
APP_PID=$!

for _ in {1..80}; do
  if ! kill -0 "$APP_PID" 2>/dev/null; then
    cat "$LOG_FILE" >&2
    exit 1
  fi
  if grep -q "state=loaded" "$STATUS_FILE" 2>/dev/null; then
    break
  fi
  sleep 0.1
done

if ! grep -q "state=loaded" "$STATUS_FILE" 2>/dev/null; then
  echo "timed out waiting for OpenTerm loaded state" >&2
  cat "$LOG_FILE" >&2
  [[ -f "$STATUS_FILE" ]] && cat "$STATUS_FILE" >&2
  exit 1
fi

if ! kill -0 "$APP_PID" 2>/dev/null; then
  cat "$LOG_FILE" >&2
  exit 1
fi

sleep "${OPENTERM_UI_SMOKE_SETTLE_SECONDS:-2}"
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

echo "OpenTerm GUI smoke passed: $SCREENSHOT ($BYTES bytes)"
