#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Packaged GUI SFTP screenshot smoke currently requires macOS" >&2
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

STATUS_FILE="${OPENTERM_PACKAGED_SFTP_STATUS:-${TMPDIR:-/tmp}/openterm-packaged-sftp.status}"
KNOWN_HOSTS_FILE="${OPENTERM_PACKAGED_SFTP_KNOWN_HOSTS:-${TMPDIR:-/tmp}/openterm-packaged-sftp.known_hosts}"
DB_FILE="${OPENTERM_PACKAGED_SFTP_DB:-${TMPDIR:-/tmp}/openterm-packaged-sftp.redb}"
SCREENSHOT="${OPENTERM_PACKAGED_SFTP_SCREENSHOT:-dist/openterm-packaged-sftp.png}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-packaged-sftp.XXXXXX.log")"
rm -f "$STATUS_FILE" "$KNOWN_HOSTS_FILE" "$DB_FILE" "$SCREENSHOT"

cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE"
}
trap cleanup EXIT

capture_openterm_window() {
  osascript -e "tell application \"System Events\" to set frontmost of first process whose unix id is $APP_PID to true" >/dev/null 2>&1 || true
  sleep 0.4

  local window_id=""
  for _front in {1..30}; do
    window_id="$(osascript -e 'tell application "System Events" to get value of attribute "AXWindowNumber" of window 1 of process "OpenTerm"' 2>/dev/null || true)"
    if [[ "$window_id" =~ ^[0-9]+$ ]]; then
      break
    fi
    sleep 0.1
  done

  if [[ "$window_id" =~ ^[0-9]+$ ]]; then
    screencapture -x -l "$window_id" "$SCREENSHOT"
    return
  fi

  local bounds=""
  bounds="$(osascript <<'APPLESCRIPT' 2>/dev/null || true
tell application "System Events"
  tell process "OpenTerm"
    set {x, y} to position of window 1
    set {w, h} to size of window 1
    return (x as integer) & "," & (y as integer) & "," & (w as integer) & "," & (h as integer)
  end tell
end tell
APPLESCRIPT
)"
  if [[ "$bounds" =~ ^[0-9]+,[0-9]+,[0-9]+,[0-9]+$ ]]; then
    screencapture -x -R "$bounds" "$SCREENSHOT"
    return
  fi

  screencapture -x "$SCREENSHOT"
}

echo "OpenTerm packaged SFTP screenshot smoke"

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_AUTO_SFTP=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_FILE" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
  "$APP_BIN" >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..180}; do
  if [[ -f "$STATUS_FILE" ]] \
    && grep -q "state=sftp_loaded" "$STATUS_FILE" \
    && grep -q "panel=SFTP" "$STATUS_FILE" \
    && grep -q "connection_editor=false" "$STATUS_FILE"; then
    sleep 0.8
    capture_openterm_window
    BYTES="$(wc -c <"$SCREENSHOT" | tr -d '[:space:]')"
    if [[ "$BYTES" -lt 50000 ]]; then
      echo "screenshot too small: $BYTES bytes at $SCREENSHOT" >&2
      exit 1
    fi
    echo "OpenTerm packaged SFTP screenshot smoke passed: screenshot=$SCREENSHOT bytes=$BYTES"
    exit 0
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

echo "timed out waiting for packaged SFTP listing" >&2
[[ -f "$STATUS_FILE" ]] && cat "$STATUS_FILE" >&2
exit 1
