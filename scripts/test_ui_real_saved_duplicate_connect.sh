#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "GUI saved-duplicate-connect smoke currently requires macOS" >&2
  exit 2
fi

source "$(dirname "$0")/real_password_env.sh"
resolve_openterm_real_password_env

STATUS_SAVE="${OPENTERM_UI_REAL_SAVED_DUP_SAVE_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-saved-dup-save.status}"
STATUS_DUP="${OPENTERM_UI_REAL_SAVED_DUP_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-saved-dup.status}"
KNOWN_HOSTS_FILE="${OPENTERM_UI_REAL_SAVED_DUP_KNOWN_HOSTS:-${TMPDIR:-/tmp}/openterm-ui-real-saved-dup.known_hosts}"
DB_FILE="${OPENTERM_UI_REAL_SAVED_DUP_DB:-${TMPDIR:-/tmp}/openterm-ui-real-saved-dup.redb}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-ui-real-saved-dup.XXXXXX.log")"
INPUT_MARKER="OPENTERM_SAVED_DUPLICATE_OK_$$"
rm -f "$STATUS_SAVE" "$STATUS_DUP" "$KNOWN_HOSTS_FILE" "$DB_FILE"

APP_PID=""
cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE"
}
trap cleanup EXIT

echo "OpenTerm GUI saved-duplicate-connect smoke"

cargo build -p openterm-app >"$LOG_FILE" 2>&1

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_SAVE_HOST=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_SAVE" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..80}; do
  if [[ -f "$STATUS_SAVE" ]] && grep -q "status=Saved host settings" "$STATUS_SAVE"; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
    APP_PID=""
    break
  fi
  if [[ -f "$STATUS_SAVE" ]] && grep -Eq "state=(ssh_error|shell_failed)" "$STATUS_SAVE"; then
    cat "$STATUS_SAVE" >&2
    exit 1
  fi
  sleep 0.25
done

if [[ -n "${APP_PID:-}" ]]; then
  echo "timed out waiting for saved host" >&2
  [[ -f "$STATUS_SAVE" ]] && cat "$STATUS_SAVE" >&2
  exit 1
fi

OPENTERM_DB_PATH="$DB_FILE" \
OPENTERM_UI_SMOKE_DUPLICATE_CONNECT=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_STATUS="$STATUS_DUP" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
OPENTERM_UI_SMOKE_INPUT_MARKER="$INPUT_MARKER" \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..180}; do
  if [[ -f "$STATUS_DUP" ]] \
    && grep -q "state=shell_output" "$STATUS_DUP" \
    && grep -q "tab=2" "$STATUS_DUP" \
    && grep -q "tabs=2" "$STATUS_DUP" \
    && grep -q "panel=Terminal" "$STATUS_DUP" \
    && grep -q "$INPUT_MARKER" "$STATUS_DUP"; then
    echo "OpenTerm GUI saved-duplicate-connect smoke passed: $STATUS_DUP"
    exit 0
  fi
  if [[ -f "$STATUS_DUP" ]] && grep -Eq "state=(ssh_error|shell_failed)" "$STATUS_DUP"; then
    cat "$STATUS_DUP" >&2
    exit 1
  fi
  if ! kill -0 "$APP_PID" 2>/dev/null; then
    cat "$LOG_FILE" >&2
    [[ -f "$STATUS_DUP" ]] && cat "$STATUS_DUP" >&2
    exit 1
  fi
  sleep 0.25
done

echo "timed out waiting for saved duplicate tab shell output" >&2
[[ -f "$STATUS_DUP" ]] && cat "$STATUS_DUP" >&2
exit 1
