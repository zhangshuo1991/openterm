# Security Policy

OpenTerm is designed as a local-first SSH client.

## Current Guarantees

- The application does not require an account.
- The application does not upload hosts, keys, passwords, or commands.
- Secret payloads should be stored through the vault API, not as plain text.
- Secret values must not be logged.

## Not Yet Complete

- Full known_hosts verification is represented in the domain model but the real
  SSH transport is not implemented in this initial slice.
- The vault implementation has unit tests, but has not had an external audit.

## Reporting

Do not file public issues for secret-handling vulnerabilities. Use a private
security channel once the public repository is created.
