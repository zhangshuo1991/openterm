#!/usr/bin/env bash
set -euo pipefail

HOST="${OPENTERM_TEST_HOST:-82.157.57.178}"
USER_NAME="${OPENTERM_TEST_USER:-ubuntu}"
PASSWORD_ENV="${OPENTERM_TEST_PASSWORD_ENV:-OPENTERM_TEST_PASSWORD}"
REMOTE_COMMAND="${OPENTERM_TEST_EXEC_COMMAND:-hostname && whoami && uname -s}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

source "$SCRIPT_DIR/real_password_env.sh"
OPENTERM_UI_REAL_PASSWORD_ENV="$PASSWORD_ENV" resolve_openterm_real_password_env

echo "OpenTerm exec smoke against $USER_NAME@$HOST"

OUTPUT="$(cargo run -p openterm-cli -- exec "$HOST" "$REMOTE_COMMAND" \
  --user "$USER_NAME" \
  --password-env "$PASSWORD_ENV")"

printf '%s\n' "$OUTPUT"
grep -q "$USER_NAME" <<<"$OUTPUT"
grep -Eq 'Linux|Darwin|FreeBSD|OpenBSD|NetBSD' <<<"$OUTPUT"

echo "OpenTerm exec smoke passed"
