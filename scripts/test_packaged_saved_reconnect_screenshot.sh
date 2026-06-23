#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Packaged GUI saved-reconnect screenshot smoke currently requires macOS" >&2
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

STATUS_SAVE="${OPENTERM_PACKAGED_SAVED_SAVE_STATUS:-${TMPDIR:-/tmp}/openterm-packaged-saved-save.status}"
STATUS_CONNECT="${OPENTERM_PACKAGED_SAVED_CONNECT_STATUS:-${TMPDIR:-/tmp}/openterm-packaged-saved-connect.status}"
KNOWN_HOSTS_FILE="${OPENTERM_PACKAGED_SAVED_KNOWN_HOSTS:-${TMPDIR:-/tmp}/openterm-packaged-saved.known_hosts}"
DB_FILE="${OPENTERM_PACKAGED_SAVED_DB:-${TMPDIR:-/tmp}/openterm-packaged-saved.redb}"
SCREENSHOT="${OPENTERM_PACKAGED_SAVED_SCREENSHOT:-dist/openterm-packaged-saved-reconnect.png}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-packaged-saved.XXXXXX.log")"
INPUT_MARKER="OPENTERM_PACKAGED_SAVED_OK_$$"
rm -f "$STATUS_SAVE" "$STATUS_CONNECT" "$KNOWN_HOSTS_FILE" "$DB_FILE" "$SCREENSHOT"

APP_PID=""
cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE"
}
trap cleanup EXIT

echo "OpenTerm packaged saved-reconnect screenshot smoke"

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_SAVE_HOST=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_SAVE" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
  "$APP_BIN" >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..100}; do
  if [[ -f "$STATUS_SAVE" ]] && grep -q "status=Saved host settings" "$STATUS_SAVE"; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
    APP_PID=""
    break
  fi
  if [[ -f "$STATUS_SAVE" ]] && grep -Eq "state=(ssh_error|shell_failed|host_save_failed)" "$STATUS_SAVE"; then
    cat "$STATUS_SAVE" >&2
    exit 1
  fi
  sleep 0.25
done

if [[ -n "${APP_PID:-}" ]]; then
  echo "timed out waiting for packaged saved host" >&2
  [[ -f "$STATUS_SAVE" ]] && cat "$STATUS_SAVE" >&2
  exit 1
fi

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_AUTO_CONNECT=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_STATUS="$STATUS_CONNECT" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
OPENTERM_UI_SMOKE_INPUT_MARKER="$INPUT_MARKER" \
  "$APP_BIN" >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..180}; do
  if [[ -f "$STATUS_CONNECT" ]] \
    && grep -q "state=shell_output" "$STATUS_CONNECT" \
    && grep -q "panel=Terminal" "$STATUS_CONNECT" \
    && grep -q "hosts=1" "$STATUS_CONNECT" \
    && grep -q "selected_host=OpenTerm test server" "$STATUS_CONNECT" \
    && grep -q "host_name=OpenTerm test server" "$STATUS_CONNECT" \
    && grep -q "$INPUT_MARKER" "$STATUS_CONNECT"; then
    osascript -e "tell application \"System Events\" to set frontmost of first process whose unix id is $APP_PID to true" >/dev/null 2>&1 || true
    sleep 0.6
    screencapture -x "$SCREENSHOT"
    BYTES="$(wc -c <"$SCREENSHOT" | tr -d '[:space:]')"
    if [[ "$BYTES" -lt 50000 ]]; then
      echo "screenshot too small: $BYTES bytes at $SCREENSHOT" >&2
      exit 1
    fi
    echo "OpenTerm packaged saved-reconnect screenshot smoke passed: screenshot=$SCREENSHOT bytes=$BYTES"
    exit 0
  fi
  if [[ -f "$STATUS_CONNECT" ]] && grep -Eq "state=(ssh_error|shell_failed)" "$STATUS_CONNECT"; then
    cat "$STATUS_CONNECT" >&2
    exit 1
  fi
  if ! kill -0 "$APP_PID" 2>/dev/null; then
    cat "$LOG_FILE" >&2
    [[ -f "$STATUS_CONNECT" ]] && cat "$STATUS_CONNECT" >&2
    exit 1
  fi
  sleep 0.25
done

echo "timed out waiting for packaged saved host reconnect output" >&2
[[ -f "$STATUS_CONNECT" ]] && cat "$STATUS_CONNECT" >&2
exit 1
