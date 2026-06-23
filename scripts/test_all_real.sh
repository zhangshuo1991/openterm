#!/usr/bin/env bash
set -euo pipefail

HOST="${OPENTERM_TEST_HOST:-82.157.57.178}"
USER_NAME="${OPENTERM_TEST_USER:-ubuntu}"
PASSWORD_ENV="${OPENTERM_TEST_PASSWORD_ENV:-OPENTERM_TEST_PASSWORD}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

if [[ -z "${!PASSWORD_ENV:-}" ]]; then
  if [[ -t 0 ]]; then
    read -rsp "Password for $USER_NAME@$HOST: " OPENTERM_TEST_PASSWORD
    echo
    export OPENTERM_TEST_PASSWORD
  else
    echo "missing password env var: $PASSWORD_ENV" >&2
    exit 2
  fi
fi

echo "OpenTerm full real smoke against $USER_NAME@$HOST"

"$SCRIPT_DIR/test_real_exec.sh"
"$SCRIPT_DIR/test_real_sftp.sh"
"$SCRIPT_DIR/test_real_forward.sh"

echo "OpenTerm full real smoke passed"
