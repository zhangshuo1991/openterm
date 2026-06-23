#!/usr/bin/env bash
set -euo pipefail

HOST="${OPENTERM_TEST_HOST:-82.157.57.178}"
USER_NAME="${OPENTERM_TEST_USER:-ubuntu}"
PASSWORD_ENV="${OPENTERM_TEST_PASSWORD_ENV:-OPENTERM_TEST_PASSWORD}"
REMOTE_ROOT="${OPENTERM_TEST_REMOTE_ROOT:-/tmp/openterm-sftp-smoke-$$}"
LOCAL_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-upload.XXXXXX")"
DOWNLOAD_FILE="$(mktemp "${TMPDIR:-/tmp}/openterm-download.XXXXXX")"

cleanup() {
  rm -f "$LOCAL_FILE" "$DOWNLOAD_FILE"
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

run_cli() {
  if [[ -n "${!PASSWORD_ENV:-}" ]]; then
    cargo run -p openterm-cli -- "$@" --user "$USER_NAME" --password-env "$PASSWORD_ENV"
  else
    printf '%s\n' "$OPENTERM_TEST_PASSWORD_STDIN" | \
      cargo run -p openterm-cli -- "$@" --user "$USER_NAME" --password-stdin
  fi
}

echo "OpenTerm SFTP smoke against $USER_NAME@$HOST"
printf 'openterm-sftp-smoke\n' > "$LOCAL_FILE"

run_cli sftp-mkdir "$HOST" "$REMOTE_ROOT"

run_cli sftp-upload "$HOST" "$LOCAL_FILE" "$REMOTE_ROOT/upload.txt"

run_cli sftp-list "$HOST" "$REMOTE_ROOT"

run_cli sftp-rename "$HOST" "$REMOTE_ROOT/upload.txt" "$REMOTE_ROOT/renamed.txt"

run_cli sftp-download "$HOST" "$REMOTE_ROOT/renamed.txt" "$DOWNLOAD_FILE"

cmp "$LOCAL_FILE" "$DOWNLOAD_FILE"

run_cli sftp-rm "$HOST" "$REMOTE_ROOT/renamed.txt"

run_cli sftp-rm "$HOST" "$REMOTE_ROOT" --dir

echo "OpenTerm SFTP smoke passed"
