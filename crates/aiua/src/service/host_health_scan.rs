//! Host-level health scan — folds the hand-rolled vps-jane cron monitors
//! (`host-sentinel.sh` / `health-monitor.sh`, openclaw-era) into the hotel's
//! own self-heal loop.
//!
//! Periodically samples host vitals (1m load, CPU busy %, memory used %, disk
//! used % of the data volume) plus a configurable set of TCP service probes
//! (e.g. Memgraph's bolt port), grades them against thresholds, and routes
//! breaches into the self-heal queue as pre-classified entries. The
//! heal-dispatcher then handles recurrence tracking, work-item filing, and
//! operator escalation (Telegram via the escalation role) with its existing
//! per-tag cooldowns — so a sustained breach alerts once an hour, not once a
//! scan.
//!
//! Pattern tags produced (all mapped to `escalate` in
//! [`ansible_mesh_core::heal_queue::heal_action_for_pattern_tag`] — no guest
//! restart fixes host pressure or a down external service):
//! - `host_load_high` — 1m load average over limit
//! - `host_cpu_high` — CPU busy % over limit (Linux only; delta of
//!   `/proc/stat` between scans)
//! - `host_mem_pressure` — memory used % over limit
//! - `host_disk_low` — data-volume disk used % over limit
//! - `service_probe_failed:{name}` — configured TCP probe did not connect
//!
//! Config lives in the `host_health.config` config node (JSON
//! [`HostHealthConfig`], re-read every cycle so thresholds/probes are
//! live-tunable via DB patch without a restart; absent node = defaults). Each
//! scan also writes a compact status snapshot to `host_health.status` for
//! operator/doctor queries.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::heal_queue::{HealQueueStorage, SqliteHealQueueStorage};

/// Config node holding the JSON [`HostHealthConfig`]; absent = defaults.
pub const CONFIG_KEY: &str = "host_health.config";
/// Config node the scan writes its latest status snapshot to.
pub const STATUS_KEY: &str = "host_health.status";
/// guest_id stamped on heal-queue rows filed by this scan.
const GUEST_ID: &str = "host-health-scan";
/// Floor on the scan interval. Must stay ≥ `HEAL_FLOOD_WINDOW_SECS` (60s) so
/// consecutive scans of a sustained breach are distinct rows the dispatcher's
/// recurrence counter can see, instead of flood-collapsing into one.
const MIN_INTERVAL_SECS: u64 = 60;
/// Let the hotel finish booting before the first sample (mirrors
/// model_catalog_sync's initial delay).
const INITIAL_DELAY_SECS: u64 = 90;

fn default_probe_timeout_secs() -> u64 {
    5
}

/// A TCP connect probe for an external service the hotel depends on (e.g.
/// Memgraph bolt). Connect success within `timeout_secs` = healthy. Preferred
/// over docker introspection: works under the systemd sandbox
/// (`ProtectSystem=strict`) and doesn't care how the service is hosted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceProbe {
    /// Short slug used in the pattern tag (`service_probe_failed:{name}`).
    pub name: String,
    /// `host:port` to TCP-connect.
    pub addr: String,
    #[serde(default = "default_probe_timeout_secs")]
    pub timeout_secs: u64,
}

/// Thresholds and probe set. Defaults mirror the retired vps-jane cron
/// monitors (load 4.0, cpu 90%, mem 92%, disk 90%).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HostHealthConfig {
    pub enabled: bool,
    pub interval_secs: u64,
    /// 1-minute load average limit.
    pub load_limit: f32,
    /// CPU busy % limit (Linux only — needs `/proc/stat`).
    pub cpu_limit_pct: f32,
    pub mem_used_limit_pct: f32,
    pub disk_used_limit_pct: f32,
    /// Absolute free-space floor (GiB). A disk breach requires BOTH used% over
    /// `disk_used_limit_pct` AND available below this floor: on APFS,
    /// `df` available excludes purgeable space (local snapshots, caches), so
    /// used% alone reads 95% on a disk with tens of GiB reclaimable —
    /// mbp-jane filed 155 false `host_disk_low critical` rows that way.
    pub disk_min_avail_gb: f32,
    pub probes: Vec<ServiceProbe>,
}

impl Default for HostHealthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 300,
            load_limit: 4.0,
            cpu_limit_pct: 90.0,
            mem_used_limit_pct: 92.0,
            disk_used_limit_pct: 90.0,
            disk_min_avail_gb: 15.0,
            probes: Vec::new(),
        }
    }
}

/// One scan's sampled vitals. Every field best-effort — a failed sampler is
/// `None` and simply isn't graded, never an error.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HostVitals {
    pub load_1m: Option<f32>,
    pub cpu_used_pct: Option<f32>,
    pub mem_used_pct: Option<f32>,
    pub disk_used_pct: Option<f32>,
    pub disk_avail_gb: Option<f32>,
}

/// Cumulative CPU jiffies from `/proc/stat`, kept across scans for the delta.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuTotals {
    pub idle: u64,
    pub total: u64,
}

/// A graded threshold breach, ready to file as a heal-queue row.
#[derive(Debug, Clone, PartialEq)]
pub struct Breach {
    pub pattern_tag: String,
    pub severity: &'static str,
    pub detail: String,
}

// ── parsers (pure, unit-tested) ──────────────────────────────────────────────

/// Used % and available GiB from `df -k <path>` output (used / (used +
/// available); ignores the rounded `Capacity`/`Use%` column so macOS and
/// Linux parse identically). Available GiB feeds the absolute free-space
/// floor — see [`HostHealthConfig::disk_min_avail_gb`].
pub fn parse_df_disk(stdout: &str) -> Option<(f32, f32)> {
    let line = stdout.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    let used: u64 = cols.get(2)?.parse().ok()?;
    let avail: u64 = cols.get(3)?.parse().ok()?;
    let total = used + avail;
    if total == 0 {
        return None;
    }
    let used_pct = used as f32 / total as f32 * 100.0;
    let avail_gb = avail as f32 / (1024.0 * 1024.0);
    Some((used_pct, avail_gb))
}

/// Used % from `/proc/meminfo` via `MemAvailable` (the kernel's own
/// reclaimable estimate — matches what `free` reports as available).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn parse_meminfo_used_pct(content: &str) -> Option<f32> {
    let mut total: Option<u64> = None;
    let mut avail: Option<u64> = None;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        }
    }
    let (total, avail) = (total?, avail?);
    if total == 0 {
        return None;
    }
    Some((total - avail.min(total)) as f32 / total as f32 * 100.0)
}

/// Used % from macOS `vm_stat`. "Free" must count everything the kernel can
/// reclaim on demand — free, speculative, inactive, and purgeable pages —
/// or a healthy Mac reads ~100% used forever (macOS deliberately keeps RAM
/// full of reclaimable cache). The old free+speculative-only accounting
/// filed `host_mem_pressure critical` on every scan cycle of every Mac
/// (155 false rows on mbp-jane by 2026-07-20), burying real heal signals.
pub fn parse_vm_stat_used_pct(stdout: &str) -> Option<f32> {
    // ONLY page-STATE lines participate. vm_stat also prints cumulative
    // event counters (Pageins, Compressions, Decompressions, Swapouts, …)
    // that grow without bound — the old parser summed every numeric line
    // into `total`, so on any uptime the counters dwarfed the states and
    // used% pinned at 100 regardless of real pressure (mbp-jane 2026-07-20:
    // parser said 100%, true usage 68.8%). Used = active + wired +
    // compressor-occupied; total = those + free + inactive + speculative.
    const USED_STATES: [&str; 3] = [
        "Pages active",
        "Pages wired down",
        "Pages occupied by compressor",
    ];
    const RECLAIMABLE_STATES: [&str; 3] = ["Pages free", "Pages inactive", "Pages speculative"];
    let mut used_pages: u64 = 0;
    let mut total_pages: u64 = 0;
    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(2, ':').collect();
        if parts.len() != 2 {
            continue;
        }
        let key = parts[0].trim();
        let val: u64 = parts[1].trim().trim_end_matches('.').parse().unwrap_or(0);
        if USED_STATES.iter().any(|s| key == *s) {
            used_pages += val;
            total_pages += val;
        } else if RECLAIMABLE_STATES.iter().any(|s| key == *s) {
            total_pages += val;
        }
    }
    if total_pages == 0 {
        return None;
    }
    Some(used_pages as f32 / total_pages as f32 * 100.0)
}

/// Used % from macOS `memory_pressure -Q`, the kernel's pressure-aware view
/// ("System-wide memory free percentage: NN%"). Unlike any page-count
/// accounting, this already discounts everything the kernel will reclaim
/// under pressure, so it tracks the signal Activity Monitor calls "memory
/// pressure" — the number worth alerting on.
pub fn parse_memory_pressure_used_pct(stdout: &str) -> Option<f32> {
    let line = stdout
        .lines()
        .find(|l| l.contains("memory free percentage"))?;
    let free: f32 = line
        .split(':')
        .nth(1)?
        .trim()
        .trim_end_matches('%')
        .trim()
        .parse()
        .ok()?;
    if !(0.0..=100.0).contains(&free) {
        return None;
    }
    Some(100.0 - free)
}

/// Aggregate CPU jiffies from the first line of `/proc/stat`. `idle` includes
/// iowait (a waiting CPU is not busy).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn parse_proc_stat_cpu(content: &str) -> Option<CpuTotals> {
    let line = content.lines().next()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let vals: Vec<u64> = fields.filter_map(|v| v.parse().ok()).collect();
    if vals.len() < 4 {
        return None;
    }
    let idle = vals[3] + vals.get(4).copied().unwrap_or(0);
    Some(CpuTotals {
        idle,
        total: vals.iter().sum(),
    })
}

/// Busy % across the interval between two `/proc/stat` samples. `None` when
/// the counters went backwards (reboot) or no time elapsed.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn cpu_used_pct_between(prev: CpuTotals, cur: CpuTotals) -> Option<f32> {
    let dt = cur.total.checked_sub(prev.total)?;
    let di = cur.idle.checked_sub(prev.idle)?;
    if dt == 0 {
        return None;
    }
    Some((dt - di.min(dt)) as f32 / dt as f32 * 100.0)
}

// ── grading ──────────────────────────────────────────────────────────────────

/// Grade sampled vitals against the configured limits. Severity escalates to
/// `critical` when the reading is deep past the limit (the point where the box
/// is about to fall over), mirroring the retired cron's SEV grading.
pub fn grade_vitals(cfg: &HostHealthConfig, v: &HostVitals) -> Vec<Breach> {
    let mut out = Vec::new();
    if let Some(load) = v.load_1m {
        if load > cfg.load_limit {
            out.push(Breach {
                pattern_tag: "host_load_high".into(),
                severity: if load >= cfg.load_limit * 1.5 {
                    "critical"
                } else {
                    "high"
                },
                detail: format!("load_1m={load:.2} > limit {:.2}", cfg.load_limit),
            });
        }
    }
    if let Some(cpu) = v.cpu_used_pct {
        if cpu > cfg.cpu_limit_pct {
            out.push(Breach {
                pattern_tag: "host_cpu_high".into(),
                severity: if cpu >= 97.0 { "critical" } else { "high" },
                detail: format!("cpu_used={cpu:.0}% > limit {:.0}%", cfg.cpu_limit_pct),
            });
        }
    }
    if let Some(mem) = v.mem_used_pct {
        if mem > cfg.mem_used_limit_pct {
            out.push(Breach {
                pattern_tag: "host_mem_pressure".into(),
                severity: if mem >= 97.0 { "critical" } else { "high" },
                detail: format!("mem_used={mem:.0}% > limit {:.0}%", cfg.mem_used_limit_pct),
            });
        }
    }
    if let Some(disk) = v.disk_used_pct {
        // Both gates must trip: used% over the limit AND absolute free space
        // under the floor. APFS `df` available excludes purgeable space, so
        // used% alone cries wolf on healthy Macs (see disk_min_avail_gb doc).
        // A missing avail sample falls back to the old %-only behavior.
        let below_floor = v
            .disk_avail_gb
            .map_or(true, |gb| gb < cfg.disk_min_avail_gb);
        if disk > cfg.disk_used_limit_pct && below_floor {
            out.push(Breach {
                pattern_tag: "host_disk_low".into(),
                severity: if disk >= 95.0 { "critical" } else { "high" },
                detail: format!(
                    "disk_used={disk:.0}% > limit {:.0}% and avail={}GB < floor {:.0}GB",
                    cfg.disk_used_limit_pct,
                    v.disk_avail_gb
                        .map_or_else(|| "?".into(), |gb| format!("{gb:.0}")),
                    cfg.disk_min_avail_gb
                ),
            });
        }
    }
    out
}

/// Breach for a failed service probe. A dependency being unreachable (e.g.
/// Memgraph — the whole lifegraph write path) is graded critical.
pub fn probe_breach(probe: &ServiceProbe) -> Breach {
    Breach {
        pattern_tag: format!("service_probe_failed:{}", probe.name),
        severity: "critical",
        detail: format!(
            "service probe '{}' failed: no TCP connect to {} within {}s",
            probe.name, probe.addr, probe.timeout_secs
        ),
    }
}

// ── sampling ─────────────────────────────────────────────────────────────────

/// Sample all vitals. Runs subprocesses (`df`, `vm_stat`, `sysctl`) — call via
/// `spawn_blocking`. `prev_cpu` is the last scan's `/proc/stat` totals.
fn sample_vitals(data_dir: &Path, prev_cpu: &mut Option<CpuTotals>) -> HostVitals {
    let load_1m = (|| -> Option<f32> {
        #[cfg(target_os = "macos")]
        {
            let out = std::process::Command::new("sysctl")
                .arg("vm.loadavg")
                .output()
                .ok()?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            let inner = stdout.split('{').nth(1)?.split('}').next()?;
            inner.split_whitespace().next()?.parse().ok()
        }
        #[cfg(not(target_os = "macos"))]
        {
            let content = std::fs::read_to_string("/proc/loadavg").ok()?;
            content.split_whitespace().next()?.parse().ok()
        }
    })();

    let disk = (|| -> Option<(f32, f32)> {
        let out = std::process::Command::new("df")
            .arg("-k")
            .arg(data_dir)
            .output()
            .ok()?;
        parse_df_disk(&String::from_utf8_lossy(&out.stdout))
    })();
    let disk_used_pct = disk.map(|(pct, _)| pct);
    let disk_avail_gb = disk.map(|(_, gb)| gb);

    let mem_used_pct = (|| -> Option<f32> {
        #[cfg(target_os = "macos")]
        {
            // `vm_stat` used% counts the page cache, which macOS keeps near
            // 100% on any healthy host — alerting on it cried wolf hourly on
            // both Mac hotels (2026-07-21). `memory_pressure -Q` reports the
            // kernel's pressure-aware free percentage instead; fall back to
            // vm_stat only when the tool is missing.
            let pressure = std::process::Command::new("memory_pressure")
                .arg("-Q")
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| {
                    parse_memory_pressure_used_pct(&String::from_utf8_lossy(&out.stdout))
                });
            if pressure.is_some() {
                return pressure;
            }
            let out = std::process::Command::new("vm_stat").output().ok()?;
            parse_vm_stat_used_pct(&String::from_utf8_lossy(&out.stdout))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let content = std::fs::read_to_string("/proc/meminfo").ok()?;
            parse_meminfo_used_pct(&content)
        }
    })();

    let cpu_used_pct = (|| -> Option<f32> {
        #[cfg(target_os = "macos")]
        {
            // No /proc/stat on macOS; load covers CPU saturation there.
            None
        }
        #[cfg(not(target_os = "macos"))]
        {
            let content = std::fs::read_to_string("/proc/stat").ok()?;
            let cur = parse_proc_stat_cpu(&content)?;
            let pct = prev_cpu.and_then(|prev| cpu_used_pct_between(prev, cur));
            *prev_cpu = Some(cur);
            pct
        }
    })();
    #[cfg(target_os = "macos")]
    let _ = prev_cpu;

    HostVitals {
        load_1m,
        cpu_used_pct,
        mem_used_pct,
        disk_used_pct,
        disk_avail_gb,
    }
}

async fn probe_ok(probe: &ServiceProbe) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(probe.timeout_secs.max(1)),
            tokio::net::TcpStream::connect(&probe.addr),
        )
        .await,
        Ok(Ok(_))
    )
}

fn load_config(graph: &GraphDomain) -> HostHealthConfig {
    match graph.get_config_value(CONFIG_KEY) {
        Ok(Some(json)) => serde_json::from_str(&json).unwrap_or_else(|e| {
            warn!("host-health-scan: malformed {CONFIG_KEY} ({e}); using defaults");
            HostHealthConfig::default()
        }),
        Ok(None) => HostHealthConfig::default(),
        Err(e) => {
            warn!("host-health-scan: config read failed ({e:#}); using defaults");
            HostHealthConfig::default()
        }
    }
}

// ── the loop ─────────────────────────────────────────────────────────────────

/// Spawn the periodic host-health loop. Bare loop ending on process exit,
/// matching the other background jobs in `main.rs`. `data_dir` is the volume
/// the disk check watches (the directory holding the hotel DB).
pub fn spawn_loop(graph: Arc<GraphDomain>, db_path: String, data_dir: PathBuf, hotel_name: String) {
    tokio::spawn(async move {
        let heal: Option<Arc<dyn HealQueueStorage>> = match SqliteHealQueueStorage::open(&db_path) {
            Ok(h) => Some(Arc::new(h)),
            Err(e) => {
                warn!("host-health-scan: heal_queue unavailable ({e:#}); will log only");
                None
            }
        };

        tokio::time::sleep(Duration::from_secs(INITIAL_DELAY_SECS)).await;
        let mut prev_cpu: Option<CpuTotals> = None;
        let mut prev_breached: BTreeSet<String> = BTreeSet::new();
        loop {
            let cfg = load_config(&graph);
            let interval = cfg.interval_secs.max(MIN_INTERVAL_SECS);
            if cfg.enabled {
                run_once(
                    &graph,
                    heal.as_ref(),
                    &cfg,
                    &data_dir,
                    &hotel_name,
                    &mut prev_cpu,
                    &mut prev_breached,
                )
                .await;
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });
}

/// One scan: sample → probe → grade → file breaches → persist status.
#[allow(clippy::too_many_arguments)]
async fn run_once(
    graph: &GraphDomain,
    heal: Option<&Arc<dyn HealQueueStorage>>,
    cfg: &HostHealthConfig,
    data_dir: &Path,
    hotel_name: &str,
    prev_cpu: &mut Option<CpuTotals>,
    prev_breached: &mut BTreeSet<String>,
) {
    let dir = data_dir.to_path_buf();
    let mut cpu_state = prev_cpu.take();
    let (vitals, cpu_state) = tokio::task::spawn_blocking(move || {
        let v = sample_vitals(&dir, &mut cpu_state);
        (v, cpu_state)
    })
    .await
    .unwrap_or_else(|e| {
        warn!("host-health-scan: sampler panicked: {e}");
        (HostVitals::default(), None)
    });
    *prev_cpu = cpu_state;

    let mut breaches = grade_vitals(cfg, &vitals);
    for probe in &cfg.probes {
        if !probe_ok(probe).await {
            breaches.push(probe_breach(probe));
        }
    }

    let breached: BTreeSet<String> = breaches.iter().map(|b| b.pattern_tag.clone()).collect();
    for recovered in prev_breached.difference(&breached) {
        info!("host-health-scan: {recovered} recovered on {hotel_name}");
    }

    for b in &breaches {
        let text = format!(
            "host-health: {} on {hotel_name} — {}",
            b.pattern_tag, b.detail
        );
        match heal {
            Some(hq) => {
                if let Err(e) = hq.push_classified(GUEST_ID, &text, b.severity, &b.pattern_tag) {
                    warn!("host-health-scan: heal push failed: {e}");
                }
            }
            None => warn!("host-health-scan alert (no heal queue): {text}"),
        }
    }

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let status = serde_json::json!({
        "last_scan_at": now_secs,
        "status": if breaches.is_empty() { "ok" } else { "breached" },
        "vitals": vitals,
        "breaches": breaches.iter().map(|b| b.pattern_tag.as_str()).collect::<Vec<_>>(),
    });
    if let Err(e) = graph.set_config_value(STATUS_KEY, &status.to_string()) {
        warn!("host-health-scan: status write failed: {e:#}");
    }

    if breaches.is_empty() {
        info!(
            load = ?vitals.load_1m,
            cpu = ?vitals.cpu_used_pct,
            mem = ?vitals.mem_used_pct,
            disk = ?vitals.disk_used_pct,
            "host-health-scan: ok"
        );
    } else {
        warn!(
            breaches = breaches.len(),
            tags = %breached.iter().cloned().collect::<Vec<_>>().join(","),
            "host-health-scan: threshold breaches filed to heal queue"
        );
    }
    *prev_breached = breached;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn df_disk_parses_both_platforms() {
        let linux = "Filesystem     1K-blocks     Used Available Use% Mounted on\n\
                     /dev/sda1      102400000 61440000  40960000  60% /\n";
        let (pct, avail_gb) = parse_df_disk(linux).unwrap();
        assert!((pct - 60.0).abs() < 0.5, "{pct}");
        assert!((avail_gb - 39.0).abs() < 0.5, "{avail_gb}");

        let macos = "Filesystem   1024-blocks      Used Available Capacity iused ifree %iused  Mounted on\n\
                     /dev/disk3s5   482797652 300000000 182797652    63%  1000  100    1%   /System/Volumes/Data\n";
        let (pct, avail_gb) = parse_df_disk(macos).unwrap();
        assert!((pct - 62.1).abs() < 1.0, "{pct}");
        assert!((avail_gb - 174.3).abs() < 1.0, "{avail_gb}");

        assert!(parse_df_disk("garbage").is_none());
    }

    #[test]
    fn memory_pressure_used_pct_parses_quick_output() {
        let stdout = "The system has 34359738368 (2097152 pages with a page size of 16384).\n\
                      System-wide memory free percentage: 35%\n";
        let pct = parse_memory_pressure_used_pct(stdout).unwrap();
        assert!((pct - 65.0).abs() < 0.1, "{pct}");

        assert!(parse_memory_pressure_used_pct("no such line").is_none());
        assert!(
            parse_memory_pressure_used_pct("System-wide memory free percentage: 250%").is_none()
        );
    }

    #[test]
    fn meminfo_used_pct_uses_memavailable() {
        let content =
            "MemTotal:       8000000 kB\nMemFree:         500000 kB\nMemAvailable:   2000000 kB\n";
        let pct = parse_meminfo_used_pct(content).unwrap();
        assert!((pct - 75.0).abs() < 0.1, "{pct}");
        assert!(parse_meminfo_used_pct("MemTotal: 0 kB\nMemAvailable: 0 kB").is_none());
        assert!(parse_meminfo_used_pct("").is_none());
    }

    #[test]
    fn proc_stat_cpu_delta() {
        let t0 = parse_proc_stat_cpu("cpu  100 0 100 700 100 0 0 0 0 0\n").unwrap();
        let t1 = parse_proc_stat_cpu("cpu  200 0 200 1200 200 0 0 0 0 0\n").unwrap();
        // dt = 800, idle delta = (1200+200)-(700+100) = 600 → busy 25%
        let pct = cpu_used_pct_between(t0, t1).unwrap();
        assert!((pct - 25.0).abs() < 0.1, "{pct}");
        // counters going backwards (reboot) → None
        assert!(cpu_used_pct_between(t1, t0).is_none());
        assert!(parse_proc_stat_cpu("intr 12345").is_none());
    }

    #[test]
    fn vm_stat_used_pct_parses() {
        let out = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                   Pages free:                    100000.\n\
                   Pages active:                  200000.\n\
                   Pages inactive:                100000.\n\
                   Pages speculative:             100000.\n";
        let pct = parse_vm_stat_used_pct(out).unwrap();
        // free = free + speculative + inactive = 300000 of 500000 → used 40%
        // (inactive pages are reclaimable — see vm_stat_counts_reclaimable_
        // pages_as_free for why they must count as free).
        assert!((pct - 40.0).abs() < 0.1, "{pct}");
    }

    #[test]
    fn grading_respects_limits_and_severity() {
        let cfg = HostHealthConfig::default();
        let ok = HostVitals {
            load_1m: Some(1.0),
            cpu_used_pct: Some(50.0),
            mem_used_pct: Some(80.0),
            disk_used_pct: Some(63.0),
            disk_avail_gb: Some(200.0),
        };
        assert!(grade_vitals(&cfg, &ok).is_empty());

        // Unsampled vitals grade nothing.
        assert!(grade_vitals(&cfg, &HostVitals::default()).is_empty());

        let bad = HostVitals {
            load_1m: Some(6.5),        // ≥ 4.0*1.5 → critical
            cpu_used_pct: Some(92.0),  // high
            mem_used_pct: Some(98.0),  // ≥97 → critical
            disk_used_pct: Some(91.0), // high (avail below floor too)
            disk_avail_gb: Some(8.0),
        };
        let breaches = grade_vitals(&cfg, &bad);
        let by_tag: std::collections::HashMap<_, _> = breaches
            .iter()
            .map(|b| (b.pattern_tag.as_str(), b.severity))
            .collect();
        assert_eq!(by_tag["host_load_high"], "critical");
        assert_eq!(by_tag["host_cpu_high"], "high");
        assert_eq!(by_tag["host_mem_pressure"], "critical");
        assert_eq!(by_tag["host_disk_low"], "high");
    }

    #[test]
    fn apfs_purgeable_space_does_not_cry_disk_low() {
        // mbp-jane 2026-07-20: APFS `df` reported 95% used with 45 GiB
        // genuinely available (purgeable snapshots inflate used%). 155 false
        // `host_disk_low critical` rows buried real heal signals. Used% over
        // the limit must NOT breach while absolute free space is above the
        // floor.
        let cfg = HostHealthConfig::default();
        let apfs = HostVitals {
            disk_used_pct: Some(95.0),
            disk_avail_gb: Some(45.0),
            ..Default::default()
        };
        assert!(grade_vitals(&cfg, &apfs).is_empty());

        // A genuinely full disk (over limit AND under floor) still breaches.
        let full = HostVitals {
            disk_used_pct: Some(96.0),
            disk_avail_gb: Some(9.0),
            ..Default::default()
        };
        let breaches = grade_vitals(&cfg, &full);
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].pattern_tag, "host_disk_low");
        assert_eq!(breaches[0].severity, "critical");
    }

    #[test]
    fn vm_stat_counts_reclaimable_pages_as_free_and_ignores_event_counters() {
        // Two false-100% bugs live here: (1) inactive/speculative pages are
        // reclaimable and must not count as used; (2) vm_stat's cumulative
        // EVENT counters (Pageins, Compressions, …) grow unboundedly with
        // uptime and must not enter the total — summing them pinned used%
        // at 100 on any host with real uptime (mbp-jane read 100% while
        // truly at 68.8%, filing host_mem_pressure critical every 5 min).
        let stdout = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                      Pages free:                   10000.\n\
                      Pages active:                 40000.\n\
                      Pages inactive:               30000.\n\
                      Pages speculative:             5000.\n\
                      Pages wired down:             10000.\n\
                      Pages purgeable:               5000.\n\
                      \"Translation faults\":     999999999.\n\
                      Pages copy-on-write:      123456789.\n\
                      Pageins:                  987654321.\n\
                      Compressions:             555555555.\n\
                      Swapouts:                  44444444.\n";
        let pct = parse_vm_stat_used_pct(stdout).unwrap();
        // used = active 40000 + wired 10000 = 50000
        // total = used + free 10000 + inactive 30000 + speculative 5000 = 95000
        // → 52.6% — counters and purgeable (a subset of other states) ignored.
        assert!((pct - 52.6).abs() < 0.5, "{pct}");
    }

    #[test]
    fn probe_breach_tag_carries_name() {
        let b = probe_breach(&ServiceProbe {
            name: "memgraph".into(),
            addr: "100.64.212.8:7687".into(),
            timeout_secs: 5,
        });
        assert_eq!(b.pattern_tag, "service_probe_failed:memgraph");
        assert_eq!(b.severity, "critical");
    }

    #[test]
    fn config_defaults_and_partial_json() {
        let cfg: HostHealthConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg, HostHealthConfig::default());
        assert!(cfg.enabled);

        let cfg: HostHealthConfig = serde_json::from_str(
            r#"{"disk_used_limit_pct": 80.0, "probes": [{"name":"memgraph","addr":"1.2.3.4:7687"}]}"#,
        )
        .unwrap();
        assert_eq!(cfg.disk_used_limit_pct, 80.0);
        assert_eq!(cfg.probes.len(), 1);
        assert_eq!(cfg.probes[0].timeout_secs, 5);
        assert_eq!(cfg.interval_secs, 300);
    }
}
