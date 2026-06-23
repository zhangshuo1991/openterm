#!/usr/bin/env bash
set -euo pipefail

HOST="${OPENTERM_TEST_HOST:-82.157.57.178}"
USER_NAME="${OPENTERM_TEST_USER:-ubuntu}"
PASSWORD_ENV="${OPENTERM_TEST_PASSWORD_ENV:-OPENTERM_TEST_PASSWORD}"
LOCAL_PORT="${OPENTERM_TEST_FORWARD_PORT:-18022}"
REMOTE_HOST="${OPENTERM_TEST_FORWARD_REMOTE_HOST:-127.0.0.1}"
REMOTE_PORT="${OPENTERM_TEST_FORWARD_REMOTE_PORT:-22}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-forward.XXXXXX.log")"

cleanup() {
  if [[ -n "${FORWARD_PID:-}" ]]; then
    kill "$FORWARD_PID" 2>/dev/null || true
    wait "$FORWARD_PID" 2>/dev/null || true
  fi
  rm -f "$LOG_FILE" "$LOG_FILE.one" "$LOG_FILE.two"
}
trap cleanup EXIT

if [[ -z "${!PASSWORD_ENV:-}" ]]; then
  if [[ -t 0 ]]; then
    read -rsp "Password for $USER_NAME@$HOST: " OPENTERM_TEST_PASSWORD_STDIN
    echo
  else
    echo "missing password env var: $PASSWORD_ENV" >&2
    exit 2
  fi
fi

echo "OpenTerm local-forward smoke against $USER_NAME@$HOST"

if [[ -n "${!PASSWORD_ENV:-}" ]]; then
  cargo run -p openterm-cli -- forward-local "$HOST" \
    --bind-host 127.0.0.1 \
    --bind-port "$LOCAL_PORT" \
    --remote-host "$REMOTE_HOST" \
    --remote-port "$REMOTE_PORT" \
    --user "$USER_NAME" \
    --password-env "$PASSWORD_ENV" >"$LOG_FILE" 2>&1 &
else
  printf '%s\n' "$OPENTERM_TEST_PASSWORD_STDIN" | \
    cargo run -p openterm-cli -- forward-local "$HOST" \
      --bind-host 127.0.0.1 \
      --bind-port "$LOCAL_PORT" \
      --remote-host "$REMOTE_HOST" \
      --remote-port "$REMOTE_PORT" \
      --user "$USER_NAME" \
      --password-stdin >"$LOG_FILE" 2>&1 &
fi
FORWARD_PID=$!

for _ in {1..40}; do
  if grep -q "listening 127.0.0.1:$LOCAL_PORT" "$LOG_FILE"; then
    break
  fi
  if ! kill -0 "$FORWARD_PID" 2>/dev/null; then
    cat "$LOG_FILE" >&2
    exit 1
  fi
  sleep 0.25
done

if ! grep -q "listening 127.0.0.1:$LOCAL_PORT" "$LOG_FILE"; then
  cat "$LOG_FILE" >&2
  echo "forward did not start" >&2
  exit 1
fi

timeout 5 bash -c "cat < /dev/tcp/127.0.0.1/$LOCAL_PORT" | grep -q "SSH-"

timeout 5 bash -c "cat < /dev/tcp/127.0.0.1/$LOCAL_PORT" >"$LOG_FILE.one" &
READ_ONE=$!
timeout 5 bash -c "cat < /dev/tcp/127.0.0.1/$LOCAL_PORT" >"$LOG_FILE.two" &
READ_TWO=$!
wait "$READ_ONE"
wait "$READ_TWO"
grep -q "SSH-" "$LOG_FILE.one"
grep -q "SSH-" "$LOG_FILE.two"

echo "OpenTerm local-forward smoke passed"
