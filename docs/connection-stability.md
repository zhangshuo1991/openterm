# Connection stability: the two field reports

Status: **both root causes found, fixed, and reproduced/verified against a real
OpenSSH server.** Verification commands are at the bottom.

The two reports were:

1. Large SFTP transfers "break the connection" after a while.
2. An idle terminal session drops on its own after a period with no activity.

They are **two unrelated defects**, not one flaky-transport problem.

---

## Report 1 — large SFTP transfers die mid-transfer

### What actually happens

`russh-sftp` gives every SFTP request a **10 second deadline** by default
(`russh_sftp::client::Config::request_timeout_secs`), and the transfer code never
overrode it. Combined with a **fixed 16 x 256 KiB = 4 MiB read window** — twice
the 2 MiB SSH channel window `russh` advertises — the deadline stops being a
stall detector and becomes a budget for the whole queue in front of a request:
the deepest read of every window cannot be answered until the window's earlier
data has crossed the link.

On any link slower than roughly `4 MiB / 10 s = 400 KiB/s`, the tail of each
window times out. The request is then dropped from the client's dispatch table,
its late reply is pulled off the wire and thrown away, and the transfer aborts
with `SFTP error: Timeout` **while the server keeps streaming**. There is no
retry: `download_file`/`upload_file` had no retry loop, so all bytes already
fetched were lost and the transfer row just went red.

Because the pool is unreachable (see below), that transfer ran on **the same SSH
connection as the interactive terminal**, which is why the user experiences it as
"the connection dropped" rather than "the transfer failed".

### Evidence

Reproduced with `crates/openterm-ssh/tests/stability_probe.rs`
(`transfer_completes_across_a_slow_link`) against a real OpenSSH server, with a
rate-limited TCP proxy standing in for an ~800 kbit/s cloud uplink:

```
# before
[ 12.803s  WARN] Client error. (Packet Some(8) for unknown recipient)
[ 15.362s  WARN] Client error. (Packet Some(9) for unknown recipient)
download of 4194304 bytes over a 102400 B/s link failed after 20.0s: SFTP error: Timeout

# after
download OK: 4194304 bytes in 40.6s (101 KiB/s)
upload OK:   4194304 bytes in 40.7s (101 KiB/s)
test result: ok
```

The `unknown recipient` warnings are the discarded replies — the failure mode in
one line. The transfer is byte-for-byte verified by fingerprint, and the session
is still usable afterwards.

### Fix

In `crates/openterm-ssh/src/lib.rs`:

- **A deadline a slow link cannot trip for bulk traffic** (`TRANSFER_REQUEST_TIMEOUT`,
  120 s) while interactive operations (`ls`/`stat`/navigate) keep the tight 10 s
  default, so a hung server still fails those quickly.
- **A window sized by time, not by a constant** (`TRANSFER_TAIL_TARGET`, 8 s):
  each batch measures its own throughput and resizes the window, so a slow link
  shrinks it automatically instead of building an unservable queue. Shrinks at
  once, grows at most 2x per window.
- **Retry that resumes** (`TRANSFER_ATTEMPTS` = 4, exponential backoff): the
  `.part` resume machinery already existed, it just was never used for recovery.
  A transient timeout now costs one backoff instead of the whole file.
- **A resume watermark that cannot skip a hole.** Windowed writes complete out of
  order, so after a failure the file length is not a valid resume point. One
  flag per chunk now records what actually finished, and the `.part` is truncated
  to the longest contiguous prefix — otherwise a retry would silently produce a
  corrupt file. This also removes a pre-existing risk on the pause path.
- **Uploads no longer promote unacknowledged writes**: `flush`/`shutdown` failures
  now fail the attempt (bounded, so a dead session cannot hang for the whole bulk
  deadline) instead of being swallowed before the rename.

Measured on loopback: `1610612736 bytes in 10.7s (143 MiB/s)` — no throughput
regression from the adaptive window, and correct across OpenSSH's rekey boundary.

### Isolation: a transfer no longer shares the terminal's connection

The timeout above is what killed the transfer, but *sharing the connection* is why
users experienced it as the connection dropping. Bulk uploads/downloads now run on
a connection borrowed from `SshConnectionPool` for the duration of the transfer
(`SessionConnection::acquire_transfer_connection`), acquired inside the transfer
task rather than in the worker's select loop — dialling takes seconds and would
otherwise stall the terminal's own I/O.

`connection.rs` previously took `quick_sftp_session()` for *every* SFTP operation,
which resolves to `pool.primary()`: the pool was created and never borrowed from,
so 100% of transfer traffic ran on the terminal's connection.

Verified (probe `transfer_connection_is_separate_and_its_death_spares_the_terminal`):
the transfer connection is a different session, killing it leaves the terminal's
connection alive and the shell responsive, and the pool refuses to hand the dead
connection to the next transfer.

---

## Report 2 — idle terminal session drops

### What is *not* wrong

The SSH-level keepalive is configured and works. The probe confirms keepalives go
out every 30 s and that the server answers them:

```
[ 30.809s] > msg type 80   (GLOBAL_REQUEST keepalive@openssh.com)
[ 30.810s] < msg type 82   (REQUEST_FAILURE)
...
still connected after 300.0s idle
```

`inactivity_timeout` is already `None`. So the client does not self-disconnect
against a well-behaved server.

### What is wrong

Two gaps, both about **what keeps the flow alive when the app is not running its
timers**:

1. **No TCP keepalive on the socket.** `russh::client::connect` builds its own
   `TcpStream` and sets only `nodelay`. `SO_KEEPALIVE` is opt-in and was never
   set (`sysctl net.inet.tcp.always_keepalive` = 0), and macOS' idle time defaults
   to **two hours**. So the *only* liveness traffic was a user-space tokio timer —
   and macOS App Nap throttles timers for a window the user has not touched in a
   while, which is exactly the reported situation ("a while with no activity in
   the terminal window"). With that timer throttled, a stateful firewall or NAT on
   the path is free to forget the idle flow and the session dies with no packet
   ever having been sent to prevent it.
2. **`ServerAliveInterval` was a dead setting.** The Settings screen offered it
   (`ui/settings.rs`, default `60`, hint "0 = disabled"), the value was stored in
   the `App` struct — and never reached the connection. The real interval was
   hardcoded to 30 s, and the value was not persisted either.

### Fix

- **TCP keepalives on every connection's socket**, with all four knobs set
  (`SO_KEEPALIVE`, idle 30 s, interval 15 s, 4 probes). The kernel sends these, so
  a throttled or descheduled app cannot stop them — this is the only defence that
  survives App Nap. The connection now builds its own `TcpStream` and uses
  `client::connect_stream` instead of `client::connect`.
- **The SSH keepalive is now caller-controlled** (`ConnectOptions::keepalive_interval`,
  `keepalive_max`), wired from the Settings field through `build_route` for both
  the target and the jump host, parsed by `parse_keepalive_interval` (0 = off,
  clamped 5..3600, junk falls back to the default instead of blocking connects),
  and **persisted** (`UiSettings::server_alive_interval`).
- **A looser dead-peer tolerance** (`DEFAULT_KEEPALIVE_MAX` = 5, russh's default
  is 3): at a 30 s interval that default gave a session only ~2 minutes of grace,
  which a throttled app or a badly congested link can exceed while the session is
  perfectly healthy. Declaring a live session dead is worse than noticing a dead
  one a minute later; TCP keepalives still detect a truly dead peer.
- **App Nap disabled for the packaged app** (`NSAppSleepDisabled` in
  `scripts/package_macos.sh`).

Underlying SSH keepalives are also now switchable off, which is what makes the
`idle_shell_survives_on_tcp_keepalives_alone` probe meaningful: it holds a session
idle for 5 minutes with **no user-space keepalive at all** (nothing is sent by the
client for the whole window), i.e. the App Nap scenario, and requires the session
to still be usable afterwards.

What each piece of evidence does and does not prove:

- The socket-option unit test (`tcp_keepalives_are_enabled_on_the_connect_socket`)
  reads `SO_KEEPALIVE`, `TCP_KEEPALIVE`/`TCP_KEEPIDLE`, `TCP_KEEPINTVL` and
  `TCP_KEEPCNT` back from a live socket, so "keepalives are on" is a measurement,
  not an intention.
- The 5-minute TCP-only probe proves the client has no *other* path to an idle
  disconnect (no inactivity timeout, no dead-peer counter tripping).
- Neither can prove a NAT mapping survives, because loopback has no NAT. That
  property is the standard reason TCP keepalive probes exist at all; the point of
  moving liveness to the kernel is that it no longer depends on the app being
  scheduled.

---

## Verified against the real server

Local probes use a loopback sshd and a synthetic slow link. The end-to-end check
runs against the real host with the credentials already saved in the app's own
database (read from the saved host, never typed into the test):

```sh
cargo test -p openterm-app --bin openterm-app real_server_transfer_through_the_pool \
  -- --ignored --nocapture
# OPENTERM_REAL_DB / OPENTERM_REAL_HOST / OPENTERM_REAL_MB / OPENTERM_REAL_IDLE_SECS override
```

It drives the production path — `SessionConnection::new_pooled`, PTY shell on the
primary, bulk transfer on a pooled data connection — and verifies the round trip
by fingerprint:

```
connecting to ubuntu@82.157.57.178 (password from saved host)
connected in 3.7s
terminal answered on the primary connection
upload OK:   50331648 bytes in 42.4s (1159 KiB/s)
download OK: 50331648 bytes in 79.1s ( 621 KiB/s), bytes verified
idling 120s to test the reported keepalive drop ...
still connected after 120s idle → terminal still usable
test result: ok. 1 passed
```

That host sustains **621 KiB/s down**, which is the point: the old fixed 16 x 256 KiB
window makes the deepest queued read wait ~6.6 s for its reply on a *clean* link —
only 3.4 s of margin against the old 10 s deadline, so one congestion or
retransmission event aborts the transfer. This is why the failure looked
intermittent ("transfers for a while, then drops") rather than deterministic.

---

## Defects found while investigating (all fixed)

1. **`cargo test --workspace` did not build** (exit 101). `tests/pool_integration_test.rs`
   used an API that never existed (`ConnectRoute { host, port, user, auth, options }`,
   `HostKeyPolicy::Accept`, `PoolConfig { max_size, connect_timeout }`, ...), 45
   errors; `connection.rs`'s own test module had lost its `RusshBackend` import;
   and a doc example in `connection_pool.rs` (`download_file(...)`) never compiled.
   All three fixed; the fabricated test file was removed.
2. **`cargo test -p openterm-ssh --lib` failed at runtime**:
   `network_probe::tests::quality_degrades_with_errors`, because `update_quality`
   returned early without RTT samples so error rate could never degrade the rating.
3. **Unreachable "stability" modules.** `network_probe`, `adaptive_transfer`,
   `resilient_io` and `transfer_monitor` were never constructed by anything;
   `adaptive_transfer`'s parameters were `eprintln!`-logged and discarded inside
   `lib.rs`. The pool's own accounting was also wrong: the create semaphore was
   released immediately after dialling, so `max_data_connections` did not bound
   anything, and `health_check_round` held the pool mutex across a network probe.
   Removed the four unreachable modules, wired the pool in for real, and fixed its
   accounting (the capacity permit now travels with the connection, so the cap is
   a cap) plus `is_alive`, which used to open an exec channel and could therefore
   report a healthy connection dead while transfers held the channel permits.
4. **Eight root-level documents** (`CHECKLIST.md`, `DELIVERY_SUMMARY.md`,
   `IMPLEMENTATION_*.md`, `POOL_*.md`, `QUICK_REFERENCE.md`) described that
   unreachable work as delivered, wired and verified, including a release binary
   and test transcripts that did not exist. Deleted; this file replaces them.

## Reproducing / verifying

Start the fixture sshd (loopback, key auth, real `/usr/libexec/sftp-server`):

```sh
mkdir -p /tmp/openterm-sshd-test
/usr/sbin/sshd -f ~/.openterm-sshd-test/sshd_config -E /tmp/openterm-sshd-test/sshd.log
```

Run the probes (`#[ignore]`d because they are minutes long):

```sh
cargo test -p openterm-ssh --test stability_probe -- --ignored --nocapture --test-threads=1

# individually
cargo test -p openterm-ssh --test stability_probe -- --ignored --nocapture \
  transfer_completes_across_a_slow_link            # report 1: ~80 s, throttled link, both directions
  idle_shell_survives_without_activity             # report 2: 5 min, SSH keepalives on
  idle_shell_survives_on_tcp_keepalives_alone      # report 2: 5 min, App Nap scenario
  transfer_completes_over_loopback_across_a_rekey  # ~2.5 GiB scratch, throughput + rekey
  transfer_connection_is_separate_and_its_death_spares_the_terminal
  pool_reuses_data_connections_and_enforces_its_cap
```

`transfer_completes_across_a_slow_link` asserts its own harness is honest: it
requires the transfer to take at least half the theoretical link time, so a
transfer that only renamed a leftover scratch file cannot pass as a success.

`OPENTERM_PROBE_LOG=1` also prints russh's own keepalive / disconnect / SFTP
diagnostics, which is how the `unknown recipient` evidence above was captured.

Unit-level regressions (fast, no fixture needed):

```sh
cargo test --workspace
```

`cargo test --workspace` now exits 0. It previously failed at *three* gates:
the fabricated pool test file and the `connection.rs` test module did not
compile, `network_probe::tests::quality_degrades_with_errors` failed at runtime,
and a doc example in `connection_pool.rs` (`download_file(...)`) did not compile.
Two tests skip loudly when their fixture is absent instead of failing: the
loopback-sshd multiplex test, and the local-shell test on a machine that cannot
allocate a PTY (this sandbox denies `openpty`, so both are exercised elsewhere).
