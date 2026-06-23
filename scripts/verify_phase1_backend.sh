#!/usr/bin/env bash
# Phase 1 real-connection verification against the live test server.
# Requires OPENTERM_TEST_PASSWORD in the environment.
set -uo pipefail
cd "$(dirname "$0")/.."

HOST=82.157.57.178
USER=ubuntu

if [ -z "${OPENTERM_TEST_PASSWORD:-}" ]; then
  echo "OPENTERM_TEST_PASSWORD not set" >&2
  exit 2
fi

echo "=== [1/3] backend exec (connect_with_route + exec) ==="
./target/debug/openterm-cli exec "$HOST" "hostname && whoami && uname -s" \
  --user "$USER" --password-env OPENTERM_TEST_PASSWORD --trust-unknown-host-keys
echo "exec rc=$?"

echo "=== [2/3] backend sftp-list (open_sftp + list_dir, the multiplex path) ==="
./target/debug/openterm-cli sftp-list "$HOST" "." \
  --user "$USER" --password-env OPENTERM_TEST_PASSWORD --trust-unknown-host-keys
echo "sftp rc=$?"

echo "=== [3/3] interactive shell (event_shell &self path) — send a marker, expect echo ==="
printf 'echo OPENTERM_SHELL_OK_%s\nexit\n' "$$" | \
  ./target/debug/openterm-cli shell "$HOST" \
  --user "$USER" --password-env OPENTERM_TEST_PASSWORD --trust-unknown-host-keys 2>&1 | \
  grep -q "OPENTERM_SHELL_OK_$$" && echo "shell echo: OK" || echo "shell echo: (check output above)"

echo "=== backend verification done ==="
