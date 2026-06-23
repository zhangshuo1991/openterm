#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Packaged GUI real-connect smoke currently requires macOS" >&2
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

STATUS_FILE="${OPENTERM_PACKAGED_REAL_STATUS:-${TMPDIR:-/tmp}/openterm-packaged-real-connect.status}"
KNOWN_HOSTS_FILE="${OPENTERM_PACKAGED_REAL_KNOWN_HOSTS:-${TMPDIR:-/tmp}/openterm-packaged-real-connect.known_hosts}"
DB_FILE="${OPENTERM_PACKAGED_REAL_DB:-${TMPDIR:-/tmp}/openterm-packaged-real-connect.redb}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-packaged-real-connect.XXXXXX.log")"
INPUT_MARKER="OPENTERM_PACKAGED_INPUT_OK_$$"
rm -f "$STATUS_FILE" "$KNOWN_HOSTS_FILE" "$DB_FILE"

cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE"
}
trap cleanup EXIT

echo "OpenTerm packaged GUI real-connect smoke"

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_AUTO_CONNECT=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_FILE" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
OPENTERM_UI_SMOKE_INPUT_MARKER="$INPUT_MARKER" \
  "$APP_BIN" >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..180}; do
  if [[ -f "$STATUS_FILE" ]] \
    && grep -q "state=shell_output" "$STATUS_FILE" \
    && grep -q "panel=Terminal" "$STATUS_FILE" \
    && grep -q "capture_keys=true" "$STATUS_FILE" \
    && grep -q "$INPUT_MARKER" "$STATUS_FILE"; then
    echo "OpenTerm packaged GUI real-connect smoke passed: $STATUS_FILE"
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

echo "timed out waiting for packaged GUI shell output" >&2
[[ -f "$STATUS_FILE" ]] && cat "$STATUS_FILE" >&2
exit 1
