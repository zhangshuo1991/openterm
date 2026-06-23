//! Remote resource monitoring: a single shell command samples Linux `/proc`
//! (plus `df`) in one round-trip, and we parse its output into a raw snapshot.
//! Rate-based metrics (CPU %, disk IO, network IO) are derived by diffing two
//! consecutive snapshots, so the server never has to `sleep` to produce them.
//!
//! Everything here is pure parsing/arithmetic — no IO — so it is unit-testable
//! against captured fixtures.

use std::time::Instant;

/// The one command we run each poll. Markers (`@@x@@`) delimit each section so
/// the parser never has to guess where one file's output ends. Stderr is
/// discarded per-section (`2>/dev/null`) so a missing file can't corrupt the
/// stream. Targets Linux; on a non-Linux host most sections are simply empty
/// and the parsed values stay at zero.
pub const SAMPLE_COMMAND: &str = "\
echo @@CPU@@; grep '^cpu' /proc/stat 2>/dev/null; \
echo @@MEM@@; cat /proc/meminfo 2>/dev/null; \
echo @@LOAD@@; cat /proc/loadavg 2>/dev/null; \
echo @@UP@@; cat /proc/uptime 2>/dev/null; \
echo @@DISK@@; cat /proc/diskstats 2>/dev/null; \
echo @@NET@@; cat /proc/net/dev 2>/dev/null; \
echo @@DF@@; df -kP / 2>/dev/null; \
echo @@END@@";

/// A raw, absolute snapshot. Counters (cpu jiffies, sectors, bytes) are
/// meaningful only as differences between two snapshots.
#[derive(Debug, Clone, Default)]
pub struct RawSample {
    /// Aggregate CPU jiffies: (total_busy+idle, idle).
    pub cpu_total: u64,
    pub cpu_idle: u64,
    /// Per-core (total, idle) jiffies, for the optional core breakdown.
    pub cpu_cores: Vec<(u64, u64)>,
    pub mem_total_kb: u64,
    pub mem_available_kb: u64,
    pub swap_total_kb: u64,
    pub swap_free_kb: u64,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    /// Running tasks / total tasks (threads), from /proc/loadavg field 4.
    pub tasks_running: u64,
    pub tasks_total: u64,
    pub uptime_secs: f64,
    /// Cumulative disk sectors read / written across physical block devices.
    pub disk_read_sectors: u64,
    pub disk_write_sectors: u64,
    /// Cumulative network bytes received / transmitted across real interfaces.
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
    /// Root filesystem usage from `df`.
    pub disk_used_kb: u64,
    pub disk_total_kb: u64,
}

/// Display-ready metrics computed from the latest sample (and the previous one,
/// for rates). All percentages are 0..=100.
#[derive(Debug, Clone, Default)]
pub struct SessionMetrics {
    pub cpu_percent: f32,
    pub per_core: Vec<f32>,
    pub mem_used_kb: u64,
    pub mem_total_kb: u64,
    pub mem_percent: f32,
    pub swap_used_kb: u64,
    pub swap_total_kb: u64,
    pub swap_percent: f32,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub tasks_running: u64,
    pub tasks_total: u64,
    pub uptime_secs: f64,
    pub disk_used_kb: u64,
    pub disk_total_kb: u64,
    pub disk_percent: f32,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
    /// True once we have a previous sample, so rate fields are meaningful.
    pub has_rates: bool,
}

const SECTOR_BYTES: u64 = 512;

/// One process row, parsed from `ps`. CPU/MEM are percentages as `ps` reports
/// them (cpu can exceed 100 on multi-core for a multithreaded process).
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub user: String,
    pub cpu: f32,
    pub mem: f32,
    pub rss_kb: u64,
    pub command: String,
}

/// Which column the process table is sorted by (descending).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSort {
    Cpu,
    Memory,
}

/// Command run on demand to list processes. Sorting is done client-side (so we
/// don't depend on `ps --sort`, which varies across distros). `user:20` widens
/// the user column so long names aren't truncated. A BusyBox `ps` without
/// `-eo` simply yields no parseable rows → the table shows "unavailable".
pub const PROCESS_COMMAND: &str = "\
echo @@PS@@; ps -eo pid,user:20,pcpu,pmem,rss,comm 2>/dev/null; \
echo @@END@@";

/// Parse `ps` output (between `@@PS@@` and `@@END@@`) into process rows.
/// Layout: `PID USER %CPU %MEM RSS COMMAND` — the first 5 columns are
/// whitespace-delimited; everything after is the command (may contain spaces).
pub fn parse_processes(stdout: &str) -> Vec<ProcessInfo> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in stdout.lines() {
        if let Some(marker) = line.strip_prefix("@@").and_then(|l| l.strip_suffix("@@")) {
            in_section = marker == "PS";
            continue;
        }
        if !in_section {
            continue;
        }
        let trimmed = line.trim_start();
        // Skip the ps header row.
        if trimmed.starts_with("PID") {
            continue;
        }
        if let Some(p) = parse_process_line(trimmed) {
            out.push(p);
        }
    }
    out
}

fn parse_process_line(line: &str) -> Option<ProcessInfo> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse::<u32>().ok()?;
    let user = fields.next()?.to_string();
    let cpu = fields.next()?.parse::<f32>().ok()?;
    let mem = fields.next()?.parse::<f32>().ok()?;
    let rss_kb = fields.next()?.parse::<u64>().ok()?;
    // The remainder of the line (after the 5th column) is the command, which
    // may itself contain spaces.
    let command = fields.collect::<Vec<_>>().join(" ");
    Some(ProcessInfo {
        pid,
        user,
        cpu,
        mem,
        rss_kb,
        command,
    })
}

/// Sort processes descending by the chosen column.
pub fn sort_processes(procs: &mut [ProcessInfo], sort: ProcessSort) {
    match sort {
        ProcessSort::Cpu => procs.sort_by(|a, b| {
            b.cpu
                .partial_cmp(&a.cpu)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        ProcessSort::Memory => procs.sort_by(|a, b| {
            b.mem
                .partial_cmp(&a.mem)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
    }
}

/// Parse the combined sample command's stdout into a [`RawSample`].
pub fn parse_sample(stdout: &str) -> RawSample {
    let mut sample = RawSample::default();
    let mut section = "";
    for line in stdout.lines() {
        if let Some(marker) = line.strip_prefix("@@").and_then(|l| l.strip_suffix("@@")) {
            section = marker_name(marker);
            continue;
        }
        match section {
            "CPU" => parse_cpu_line(line, &mut sample),
            "MEM" => parse_mem_line(line, &mut sample),
            "LOAD" => parse_load_line(line, &mut sample),
            "UP" => parse_uptime_line(line, &mut sample),
            "DISK" => parse_diskstat_line(line, &mut sample),
            "NET" => parse_net_line(line, &mut sample),
            "DF" => parse_df_line(line, &mut sample),
            _ => {}
        }
    }
    sample
}

/// `@@CPU@@` -> `"CPU"`; an unknown marker maps to itself.
fn marker_name(marker: &str) -> &str {
    marker
}

fn parse_cpu_line(line: &str, sample: &mut RawSample) {
    // "cpu  u n s idle iowait irq softirq steal ..."  (aggregate)
    // "cpu0 ..." (per-core)
    let mut it = line.split_whitespace();
    let Some(tag) = it.next() else { return };
    if !tag.starts_with("cpu") {
        return;
    }
    let vals: Vec<u64> = it.filter_map(|v| v.parse::<u64>().ok()).collect();
    if vals.len() < 4 {
        return;
    }
    // idle = field 4 (index 3) + iowait (index 4) if present.
    let idle = vals[3] + vals.get(4).copied().unwrap_or(0);
    let total: u64 = vals.iter().sum();
    if tag == "cpu" {
        sample.cpu_total = total;
        sample.cpu_idle = idle;
    } else {
        sample.cpu_cores.push((total, idle));
    }
}

fn parse_mem_line(line: &str, sample: &mut RawSample) {
    let mut it = line.split_whitespace();
    let Some(key) = it.next() else { return };
    let Some(val) = it.next().and_then(|v| v.parse::<u64>().ok()) else {
        return;
    };
    match key {
        "MemTotal:" => sample.mem_total_kb = val,
        "MemAvailable:" => sample.mem_available_kb = val,
        "SwapTotal:" => sample.swap_total_kb = val,
        "SwapFree:" => sample.swap_free_kb = val,
        _ => {}
    }
}

fn parse_load_line(line: &str, sample: &mut RawSample) {
    // "0.00 0.01 0.05 1/234 5678"
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < 4 {
        return;
    }
    sample.load1 = f[0].parse().unwrap_or(0.0);
    sample.load5 = f[1].parse().unwrap_or(0.0);
    sample.load15 = f[2].parse().unwrap_or(0.0);
    if let Some((run, total)) = f[3].split_once('/') {
        sample.tasks_running = run.parse().unwrap_or(0);
        sample.tasks_total = total.parse().unwrap_or(0);
    }
}

fn parse_uptime_line(line: &str, sample: &mut RawSample) {
    if let Some(first) = line.split_whitespace().next() {
        sample.uptime_secs = first.parse().unwrap_or(0.0);
    }
}

fn parse_diskstat_line(line: &str, sample: &mut RawSample) {
    // Fields: major minor name reads rd_merged rd_sectors ... wr_sectors(=9) ...
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < 10 {
        return;
    }
    let name = f[2];
    // Skip partitions and virtual devices; count whole disks only so we don't
    // double-count (sda + sda1 + sda2). Accept sd*, vd*, nvme*n*, xvd*, hd*.
    if !is_whole_disk(name) {
        return;
    }
    sample.disk_read_sectors += f[5].parse::<u64>().unwrap_or(0);
    sample.disk_write_sectors += f[9].parse::<u64>().unwrap_or(0);
}

/// Heuristic: a whole physical disk (not a partition). e.g. "sda" yes,
/// "sda1" no; "nvme0n1" yes, "nvme0n1p1" no.
fn is_whole_disk(name: &str) -> bool {
    let is_block = name.starts_with("sd")
        || name.starts_with("vd")
        || name.starts_with("xvd")
        || name.starts_with("hd")
        || name.starts_with("nvme");
    if !is_block {
        return false;
    }
    // Reject names ending in a partition suffix.
    if name.starts_with("nvme") {
        // nvme0n1 = disk; nvme0n1p1 = partition.
        !name.contains('p')
            || !name
                .rsplit('p')
                .next()
                .unwrap_or("")
                .chars()
                .all(|c| c.is_ascii_digit())
    } else {
        // sda = disk; sda1 = partition (trailing digit).
        !name
            .chars()
            .last()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    }
}

fn parse_net_line(line: &str, sample: &mut RawSample) {
    // "  eth0: rx_bytes ... tx_bytes ..."  rx=field1, tx=field9 after the colon.
    let Some((iface, rest)) = line.split_once(':') else {
        return;
    };
    let iface = iface.trim();
    if iface == "lo" || iface.is_empty() {
        return;
    }
    let nums: Vec<u64> = rest
        .split_whitespace()
        .filter_map(|v| v.parse::<u64>().ok())
        .collect();
    if nums.len() < 9 {
        return;
    }
    sample.net_rx_bytes += nums[0];
    sample.net_tx_bytes += nums[8];
}

fn parse_df_line(line: &str, sample: &mut RawSample) {
    // "Filesystem 1024-blocks Used Available Capacity Mounted"; skip header.
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < 6 || f[1] == "1024-blocks" {
        return;
    }
    if let (Ok(total), Ok(used)) = (f[1].parse::<u64>(), f[2].parse::<u64>()) {
        sample.disk_total_kb = total;
        sample.disk_used_kb = used;
    }
}

/// Build display metrics from the current sample and, when available, the
/// previous (sample, time) pair for rate computation.
pub fn compute(
    curr: &RawSample,
    prev: Option<&(Instant, RawSample)>,
    now: Instant,
) -> SessionMetrics {
    let mut m = SessionMetrics {
        mem_total_kb: curr.mem_total_kb,
        mem_used_kb: curr.mem_total_kb.saturating_sub(curr.mem_available_kb),
        swap_total_kb: curr.swap_total_kb,
        swap_used_kb: curr.swap_total_kb.saturating_sub(curr.swap_free_kb),
        load1: curr.load1,
        load5: curr.load5,
        load15: curr.load15,
        tasks_running: curr.tasks_running,
        tasks_total: curr.tasks_total,
        uptime_secs: curr.uptime_secs,
        disk_used_kb: curr.disk_used_kb,
        disk_total_kb: curr.disk_total_kb,
        ..Default::default()
    };
    m.mem_percent = pct(m.mem_used_kb, m.mem_total_kb);
    m.swap_percent = pct(m.swap_used_kb, m.swap_total_kb);
    m.disk_percent = pct(m.disk_used_kb, m.disk_total_kb);

    if let Some((prev_t, prev)) = prev {
        let dt = now.duration_since(*prev_t).as_secs_f64().max(0.001);
        m.cpu_percent = cpu_delta_pct(prev.cpu_total, prev.cpu_idle, curr.cpu_total, curr.cpu_idle);
        if prev.cpu_cores.len() == curr.cpu_cores.len() {
            m.per_core = curr
                .cpu_cores
                .iter()
                .zip(&prev.cpu_cores)
                .map(|(&(ct, ci), &(pt, pi))| cpu_delta_pct(pt, pi, ct, ci))
                .collect();
        }
        m.disk_read_bps =
            rate(prev.disk_read_sectors, curr.disk_read_sectors, dt) * SECTOR_BYTES as f64;
        m.disk_write_bps =
            rate(prev.disk_write_sectors, curr.disk_write_sectors, dt) * SECTOR_BYTES as f64;
        m.net_rx_bps = rate(prev.net_rx_bytes, curr.net_rx_bytes, dt);
        m.net_tx_bps = rate(prev.net_tx_bytes, curr.net_tx_bytes, dt);
        m.has_rates = true;
    }
    m
}

fn pct(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64 * 100.0) as f32
    }
}

fn rate(prev: u64, curr: u64, dt: f64) -> f64 {
    curr.saturating_sub(prev) as f64 / dt
}

fn cpu_delta_pct(prev_total: u64, prev_idle: u64, curr_total: u64, curr_idle: u64) -> f32 {
    let total_d = curr_total.saturating_sub(prev_total);
    let idle_d = curr_idle.saturating_sub(prev_idle);
    if total_d == 0 {
        0.0
    } else {
        ((total_d.saturating_sub(idle_d)) as f64 / total_d as f64 * 100.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const FIXTURE: &str = "\
@@CPU@@
cpu  1000 0 500 8000 500 0 0 0 0 0
cpu0 500 0 250 4000 250 0 0 0 0 0
cpu1 500 0 250 4000 250 0 0 0 0 0
@@MEM@@
MemTotal:        4000000 kB
MemFree:          500000 kB
MemAvailable:    1000000 kB
SwapTotal:       2000000 kB
SwapFree:        1500000 kB
@@LOAD@@
0.50 0.40 0.30 2/345 9999
@@UP@@
123456.78 654321.00
@@DISK@@
   8       0 sda 100 0 2000 0 200 0 4000 0 0 0 0
   8       1 sda1 50 0 1000 0 100 0 2000 0 0 0 0
@@NET@@
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 100 1 0 0 0 0 0 0 100 1 0 0 0 0 0 0
  eth0: 10000 50 0 0 0 0 0 0 20000 60 0 0 0 0 0 0
@@DF@@
Filesystem     1024-blocks    Used Available Capacity Mounted on
/dev/sda1         50000000 20000000  30000000      40% /
@@END@@";

    #[test]
    fn parses_all_sections() {
        let s = parse_sample(FIXTURE);
        assert_eq!(s.cpu_total, 10000);
        assert_eq!(s.cpu_idle, 8500);
        assert_eq!(s.cpu_cores.len(), 2);
        assert_eq!(s.mem_total_kb, 4_000_000);
        assert_eq!(s.mem_available_kb, 1_000_000);
        assert_eq!(s.swap_total_kb, 2_000_000);
        assert_eq!(s.swap_free_kb, 1_500_000);
        assert_eq!(s.load1, 0.50);
        assert_eq!(s.tasks_running, 2);
        assert_eq!(s.tasks_total, 345);
        assert!((s.uptime_secs - 123456.78).abs() < 0.01);
        // Only the whole disk sda counts, not sda1.
        assert_eq!(s.disk_read_sectors, 2000);
        assert_eq!(s.disk_write_sectors, 4000);
        // Only eth0 counts, not lo.
        assert_eq!(s.net_rx_bytes, 10000);
        assert_eq!(s.net_tx_bytes, 20000);
        assert_eq!(s.disk_total_kb, 50_000_000);
        assert_eq!(s.disk_used_kb, 20_000_000);
    }

    #[test]
    fn computes_percentages_without_prev() {
        let s = parse_sample(FIXTURE);
        let m = compute(&s, None, Instant::now());
        assert_eq!(m.mem_used_kb, 3_000_000);
        assert!((m.mem_percent - 75.0).abs() < 0.1);
        assert!((m.swap_percent - 25.0).abs() < 0.1);
        assert!((m.disk_percent - 40.0).abs() < 0.1);
        assert!(!m.has_rates);
        assert_eq!(m.cpu_percent, 0.0);
    }

    #[test]
    fn computes_rates_from_two_samples() {
        let prev = parse_sample(FIXTURE);
        let t0 = Instant::now();
        // Second sample: +1000 total cpu jiffies, +200 idle => 80% busy.
        // +1000 read sectors, +2000 write sectors over 2s.
        // +20000 rx bytes over 2s = 10000 B/s.
        let next = "\
@@CPU@@
cpu  2000 0 800 8200 500 0 0 0 0 0
cpu0 1000 0 400 4100 250 0 0 0 0 0
cpu1 1000 0 400 4100 250 0 0 0 0 0
@@DISK@@
   8       0 sda 200 0 3000 0 400 0 6000 0 0 0 0
@@NET@@
  eth0: 30000 90 0 0 0 0 0 0 40000 100 0 0 0 0 0 0
@@DF@@
Filesystem     1024-blocks    Used Available Capacity Mounted on
/dev/sda1         50000000 20000000  30000000      40% /
@@END@@";
        let curr = parse_sample(next);
        let m = compute(&curr, Some(&(t0, prev)), t0 + Duration::from_secs(2));
        assert!(m.has_rates);
        // prev total=10000 idle=8500; curr total=11500 idle=8700.
        // total_d=1500, idle_d=200 => busy=1300 => 86.67%.
        assert!((m.cpu_percent - 86.67).abs() < 0.5, "cpu={}", m.cpu_percent);
        assert_eq!(m.per_core.len(), 2);
        // disk read: (3000-2000) sectors * 512 / 2s = 256000 B/s.
        assert!(
            (m.disk_read_bps - 256_000.0).abs() < 1.0,
            "rd={}",
            m.disk_read_bps
        );
        // net rx: (30000-10000)/2 = 10000 B/s.
        assert!((m.net_rx_bps - 10_000.0).abs() < 1.0, "rx={}", m.net_rx_bps);
    }

    #[test]
    fn whole_disk_detection() {
        assert!(is_whole_disk("sda"));
        assert!(!is_whole_disk("sda1"));
        assert!(is_whole_disk("nvme0n1"));
        assert!(!is_whole_disk("nvme0n1p1"));
        assert!(is_whole_disk("vda"));
        assert!(!is_whole_disk("vda2"));
        assert!(!is_whole_disk("loop0"));
    }

    const PS_FIXTURE: &str = "\
@@PS@@
    PID USER                 %CPU %MEM   RSS COMMAND
      1 root                  0.0  0.1  1680 systemd
   1234 root                 45.2  2.1 348000 dockerd
   5678 www-data              3.4 12.5 410000 nginx: worker process
   9999 postgres              1.1  8.0 260000 postgres
@@END@@";

    #[test]
    fn parses_ps_rows() {
        let procs = parse_processes(PS_FIXTURE);
        assert_eq!(procs.len(), 4);
        assert_eq!(procs[0].pid, 1);
        assert_eq!(procs[1].pid, 1234);
        assert_eq!(procs[1].user, "root");
        assert!((procs[1].cpu - 45.2).abs() < 0.01);
        assert!((procs[1].mem - 2.1).abs() < 0.01);
        assert_eq!(procs[1].rss_kb, 348000);
        assert_eq!(procs[1].command, "dockerd");
        // Command with spaces survives.
        assert_eq!(procs[2].command, "nginx: worker process");
    }

    #[test]
    fn sorts_processes_descending() {
        let mut procs = parse_processes(PS_FIXTURE);
        sort_processes(&mut procs, ProcessSort::Cpu);
        assert_eq!(procs[0].pid, 1234); // 45.2% cpu
        sort_processes(&mut procs, ProcessSort::Memory);
        assert_eq!(procs[0].pid, 5678); // 12.5% mem
    }

    #[test]
    fn busybox_ps_yields_nothing_parseable() {
        // No @@PS@@ section / unparseable → empty, no panic.
        assert!(parse_processes("@@PS@@\nsome junk\n@@END@@").is_empty());
        assert!(parse_processes("").is_empty());
    }
}
