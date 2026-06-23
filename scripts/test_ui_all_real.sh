#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "GUI full real smoke currently requires macOS" >&2
  exit 2
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

echo "OpenTerm full GUI real smoke"

run_step() {
  local label="$1"
  shift
  echo "==> $label"
  "$@"
}

run_step "settings persistence" "$SCRIPT_DIR/test_ui_settings_persistence.sh"

source "$SCRIPT_DIR/real_password_env.sh"
resolve_openterm_real_password_env

run_step "connect" "$SCRIPT_DIR/test_ui_real_connect.sh"
run_step "CJK terminal" "$SCRIPT_DIR/test_ui_real_cjk_terminal.sh"
run_step "terminal overflow" "$SCRIPT_DIR/test_ui_real_terminal_overflow.sh"
run_step "known_hosts reuse" "$SCRIPT_DIR/test_ui_real_known_hosts_reuse.sh"
run_step "saved reconnect" "$SCRIPT_DIR/test_ui_real_saved_reconnect.sh"
run_step "saved edit connect" "$SCRIPT_DIR/test_ui_real_saved_edit_connect.sh"
run_step "saved disconnect reconnect" "$SCRIPT_DIR/test_ui_real_saved_disconnect_reconnect.sh"
run_step "saved duplicate tab connect" "$SCRIPT_DIR/test_ui_real_saved_duplicate_connect.sh"
run_step "duplicate tab connect" "$SCRIPT_DIR/test_ui_real_duplicate_connect.sh"
run_step "SFTP" "$SCRIPT_DIR/test_ui_real_sftp.sh"
run_step "forward" "$SCRIPT_DIR/test_ui_real_forward.sh"
run_step "resize" "$SCRIPT_DIR/test_ui_real_resize.sh"

echo "OpenTerm full GUI real smoke passed"
