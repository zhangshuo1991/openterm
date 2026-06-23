#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "GUI known_hosts reuse smoke currently requires macOS" >&2
  exit 2
fi

source "$(dirname "$0")/real_password_env.sh"
resolve_openterm_real_password_env

STATUS_FIRST="${OPENTERM_UI_REAL_KNOWN_HOSTS_FIRST_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-known-hosts-first.status}"
STATUS_SECOND="${OPENTERM_UI_REAL_KNOWN_HOSTS_SECOND_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-known-hosts-second.status}"
KNOWN_HOSTS_FILE="${OPENTERM_UI_REAL_KNOWN_HOSTS_REUSE_FILE:-${TMPDIR:-/tmp}/openterm-ui-real-known-hosts-reuse.known_hosts}"
DB_FIRST="${OPENTERM_UI_REAL_KNOWN_HOSTS_FIRST_DB:-${TMPDIR:-/tmp}/openterm-ui-real-known-hosts-first.redb}"
DB_SECOND="${OPENTERM_UI_REAL_KNOWN_HOSTS_SECOND_DB:-${TMPDIR:-/tmp}/openterm-ui-real-known-hosts-second.redb}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-ui-real-known-hosts.XXXXXX.log")"
FIRST_MARKER="OPENTERM_KNOWN_HOSTS_FIRST_OK_$$"
SECOND_MARKER="OPENTERM_KNOWN_HOSTS_SECOND_OK_$$"
rm -f "$STATUS_FIRST" "$STATUS_SECOND" "$KNOWN_HOSTS_FILE" "$DB_FIRST" "$DB_SECOND"

APP_PID=""
cleanup() {
  if [[ -n "${APP_PID:-}" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE"
}
trap cleanup EXIT

echo "OpenTerm GUI known_hosts reuse smoke"

cargo build -p openterm-app >"$LOG_FILE" 2>&1

OPENTERM_DB_PATH="$DB_FIRST" \
OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_AUTO_CONNECT=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_FIRST" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
OPENTERM_UI_SMOKE_INPUT_MARKER="$FIRST_MARKER" \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..180}; do
  if [[ -f "$STATUS_FIRST" ]] \
    && grep -q "state=shell_output" "$STATUS_FIRST" \
    && grep -q "state=host_key_required" "$STATUS_FIRST" \
    && grep -q "state=host_key_auto_trust" "$STATUS_FIRST" \
    && grep -q "$FIRST_MARKER" "$STATUS_FIRST" \
    && [[ -s "$KNOWN_HOSTS_FILE" ]]; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
    APP_PID=""
    break
  fi
  if [[ -f "$STATUS_FIRST" ]] && grep -Eq "state=(ssh_error|shell_failed)" "$STATUS_FIRST"; then
    cat "$STATUS_FIRST" >&2
    exit 1
  fi
  if ! kill -0 "$APP_PID" 2>/dev/null; then
    cat "$LOG_FILE" >&2
    [[ -f "$STATUS_FIRST" ]] && cat "$STATUS_FIRST" >&2
    exit 1
  fi
  sleep 0.25
done

if [[ -n "${APP_PID:-}" ]]; then
  echo "timed out waiting for first known_hosts trust" >&2
  [[ -f "$STATUS_FIRST" ]] && cat "$STATUS_FIRST" >&2
  exit 1
fi

OPENTERM_DB_PATH="$DB_SECOND" \
OPENTERM_UI_SMOKE_PREFILL_TEST_SERVER=1 \
OPENTERM_UI_SMOKE_AUTO_CONNECT=1 \
OPENTERM_UI_SMOKE_AUTO_TRUST_HOST_KEY=1 \
OPENTERM_UI_SMOKE_PASSWORD_ENV="$PASSWORD_ENV" \
OPENTERM_UI_SMOKE_STATUS="$STATUS_SECOND" \
OPENTERM_UI_SMOKE_KNOWN_HOSTS="$KNOWN_HOSTS_FILE" \
OPENTERM_UI_SMOKE_INPUT_MARKER="$SECOND_MARKER" \
  target/debug/openterm-app >>"$LOG_FILE" 2>&1 &
APP_PID=$!

for _ in {1..180}; do
  if [[ -f "$STATUS_SECOND" ]] \
    && grep -q "state=shell_output" "$STATUS_SECOND" \
    && grep -q "$SECOND_MARKER" "$STATUS_SECOND"; then
    if grep -q "state=host_key_required" "$STATUS_SECOND"; then
      echo "second connect unexpectedly required host key approval" >&2
      cat "$STATUS_SECOND" >&2
      exit 1
    fi
    echo "OpenTerm GUI known_hosts reuse smoke passed: $KNOWN_HOSTS_FILE"
    exit 0
  fi
  if [[ -f "$STATUS_SECOND" ]] && grep -Eq "state=(ssh_error|shell_failed)" "$STATUS_SECOND"; then
    cat "$STATUS_SECOND" >&2
    exit 1
  fi
  if ! kill -0 "$APP_PID" 2>/dev/null; then
    cat "$LOG_FILE" >&2
    [[ -f "$STATUS_SECOND" ]] && cat "$STATUS_SECOND" >&2
    exit 1
  fi
  sleep 0.25
done

echo "timed out waiting for known_hosts reuse connect output" >&2
[[ -f "$STATUS_SECOND" ]] && cat "$STATUS_SECOND" >&2
exit 1
