#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "GUI saved-edit-connect smoke currently requires macOS" >&2
  exit 2
fi

source "$(dirname "$0")/real_password_env.sh"
resolve_openterm_real_password_env

STATUS_SAVE="${OPENTERM_UI_REAL_EDIT_SAVE_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-edit-save.status}"
STATUS_EDIT="${OPENTERM_UI_REAL_EDIT_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-edit.status}"
STATUS_CONNECT="${OPENTERM_UI_REAL_EDIT_CONNECT_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-edit-connect.status}"
KNOWN_HOSTS_FILE="${OPENTERM_UI_REAL_EDIT_KNOWN_HOSTS:-${TMPDIR:-/tmp}/openterm-ui-real-edit.known_hosts}"
DB_FILE="${OPENTERM_UI_REAL_EDIT_DB:-${TMPDIR:-/tmp}/openterm-ui-real-edit.redb}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-ui-real-edit.XXXXXX.log")"
EDITED_NAME="OpenTerm edited smoke $$"
INPUT_MARKER="OPENTERM_SAVED_EDIT_CONNECT_OK_$$"
rm -f "$STATUS_SAVE" "$STATUS_EDIT" "$STATUS_CONNECT" "$KNOWN_HOSTS_FILE" "$DB_FILE"

APP_PID=""
cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE"
}
trap cleanup EXIT

wait_for_saved_status() {
  local status_file="$1"
  local label="$2"
  for _ in {1..80}; do
    if [[ -f "$status_file" ]] && grep -q "status=Saved host settings" "$status_file"; then
      kill "$APP_PID" 2>/dev/null || true
      wait "$APP_PID" 2>/dev/null || true
      APP_PID=""
      return 0
    fi
    if [[ -f "$status_file" ]] && grep -Eq "state=(ssh_error|shell_failed|host_save_failed)" "$status_file"; then
      cat "$status_file" >&2
      exit 1
    fi
    sleep 0.25
  done

  echo "timed out waiting for $label" >&2
  [[ -f "$status_file" ]] && cat "$status_file" >&2
  exit 1
}

echo "OpenTerm GUI saved-edit-connect smoke"

cargo build -p openterm-app >"$LOG_FILE" 2>&1

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_SAVE_HOST=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_SAVE" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!
wait_for_saved_status "$STATUS_SAVE" "initial saved host"

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_SAVE_HOST=1 \
OPENTERM_UI_SMOKE_EDIT_HOST_NAME="$EDITED_NAME" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_EDIT" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!
wait_for_saved_status "$STATUS_EDIT" "edited saved host"

if ! grep -q "$EDITED_NAME" "$STATUS_EDIT"; then
  echo "edited host name did not appear in smoke status" >&2
  cat "$STATUS_EDIT" >&2
  exit 1
fi

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_AUTO_CONNECT=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_STATUS="$STATUS_CONNECT" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
OPENTERM_UI_SMOKE_INPUT_MARKER="$INPUT_MARKER" \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..180}; do
  if [[ -f "$STATUS_CONNECT" ]] \
    && grep -q "state=shell_output" "$STATUS_CONNECT" \
    && grep -q "panel=Terminal" "$STATUS_CONNECT" \
    && grep -q "host_name=$EDITED_NAME" "$STATUS_CONNECT" \
    && grep -q "$INPUT_MARKER" "$STATUS_CONNECT"; then
    echo "OpenTerm GUI saved-edit-connect smoke passed: $STATUS_CONNECT"
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

echo "timed out waiting for edited saved host connect output" >&2
[[ -f "$STATUS_CONNECT" ]] && cat "$STATUS_CONNECT" >&2
exit 1
