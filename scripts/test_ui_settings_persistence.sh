#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "GUI settings persistence smoke currently requires macOS" >&2
  exit 2
fi

DB_FILE="${OPENTERM_UI_SETTINGS_DB:-${TMPDIR:-/tmp}/openterm-ui-settings.redb}"
SET_STATUS="${OPENTERM_UI_SETTINGS_SET_STATUS:-${TMPDIR:-/tmp}/openterm-ui-settings-set.status}"
LOAD_STATUS="${OPENTERM_UI_SETTINGS_LOAD_STATUS:-${TMPDIR:-/tmp}/openterm-ui-settings-load.status}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-ui-settings.XXXXXX.log")"
rm -f "$DB_FILE" "$SET_STATUS" "$LOAD_STATUS"

cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE"
}
trap cleanup EXIT

wait_for_status() {
  local file="$1"
  local pattern="$2"
  for _ in {1..100}; do
    if [[ -f "$file" ]] && grep -q "$pattern" "$file"; then
      return 0
    fi
    if ! kill -0 "$APP_PID" 2>/dev/null; then
      cat "$LOG_FILE" >&2
      [[ -f "$file" ]] && cat "$file" >&2
      return 1
    fi
    sleep 0.2
  done
  echo "timed out waiting for $pattern in $file" >&2
  [[ -f "$file" ]] && cat "$file" >&2
  return 1
}

echo "OpenTerm GUI settings persistence smoke"

cargo build -p openterm-app >"$LOG_FILE" 2>&1

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_STATUS="$SET_STATUS" \
OPENTERM_UI_SMOKE_SET_THEME=light \
OPENTERM_UI_SMOKE_SET_FONT_SIZE=19 \
OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=0 \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!

wait_for_status "$SET_STATUS" "state=loaded"
kill "$APP_PID" 2>/dev/null || true
wait "$APP_PID" 2>/dev/null || true
APP_PID=""

if ! grep -q "theme=light" "$SET_STATUS" || ! grep -q "font_size=19" "$SET_STATUS"; then
  cat "$SET_STATUS" >&2
  echo "settings were not written during first launch" >&2
  exit 1
fi

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_STATUS="$LOAD_STATUS" \
OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=0 \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!

wait_for_status "$LOAD_STATUS" "state=loaded"

if ! grep -q "theme=light" "$LOAD_STATUS" || ! grep -q "font_size=19" "$LOAD_STATUS"; then
  cat "$LOAD_STATUS" >&2
  echo "settings were not restored during second launch" >&2
  exit 1
fi

echo "OpenTerm GUI settings persistence smoke passed: $DB_FILE"
