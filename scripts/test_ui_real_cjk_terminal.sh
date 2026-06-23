#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
STATUS_FILE="${OPENTERM_UI_REAL_CJK_STATUS:-${TMPDIR:-/tmp}/openterm-ui-real-cjk.status}"
MARKER="${OPENTERM_UI_REAL_CJK_MARKER:-OPENTERM_中文_宽字符_OK_$$}"

OPENTERM_UI_REAL_STATUS="$STATUS_FILE" \
OPENTERM_UI_REAL_INPUT_MARKER="$MARKER" \
  "$SCRIPT_DIR/test_ui_real_connect.sh"

if ! grep -q "$MARKER" "$STATUS_FILE"; then
  echo "CJK marker did not round-trip through terminal output" >&2
  cat "$STATUS_FILE" >&2
  exit 1
fi

echo "OpenTerm GUI CJK terminal smoke passed: $STATUS_FILE"
