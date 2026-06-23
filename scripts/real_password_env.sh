#!/usr/bin/env bash

resolve_openterm_real_password_env() {
  PASSWORD_ENV="${OPENTERM_UI_REAL_PASSWORD_ENV:-OPENTERM_TEST_PASSWORD}"
  if [[ -n "${!PASSWORD_ENV:-}" ]]; then
    export "$PASSWORD_ENV"
    return 0
  fi

  if [[ -t 0 ]]; then
    local password_value
    read -r -s -p "OpenTerm test SSH password: " password_value
    printf '\n'
    if [[ -z "$password_value" ]]; then
      echo "empty password; aborting" >&2
      return 2
    fi
    printf -v "$PASSWORD_ENV" '%s' "$password_value"
    export "$PASSWORD_ENV"
    unset password_value
    return 0
  fi

  echo "missing password env var: $PASSWORD_ENV" >&2
  echo "Run interactively or export $PASSWORD_ENV before starting this smoke test." >&2
  return 2
}
