use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ansible_mesh_core::{ExposureTier, ListenerProfile, PerimeterSnapshot};
use perimeter_core::classifier::classify_bind_addr;
use perimeter_core::service::{PerimeterEvent, PerimeterService};
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Listener declarations the hotel passes in at startup.
/// Port 0 is valid for UDS/IPC which has no port.
#[derive(Debug, Clone)]
pub struct ListenerDecl {
    pub purpose: &'static str,
    pub bind_addr: IpAddr,
    pub port: u16,
    pub iface: Option<String>,
}

/// Concrete PerimeterService implementation for the hotel daemon.
///
/// Derives the snapshot at construction time from declared listener bindings plus
/// live interface probes. A background task (spawned by the caller) calls `refresh`
/// on a timer or when OS network-change signals fire.
pub struct HotelPerimeterService {
    snapshot: RwLock<PerimeterSnapshot>,
    listeners: Vec<ListenerDecl>,
    tx: broadcast::Sender<PerimeterEvent>,
}

impl HotelPerimeterService {
    pub fn new(listeners: Vec<ListenerDecl>) -> Arc<Self> {
        let (tx, _) = broadcast::channel(32);
        let svc = Arc::new(Self {
            snapshot: RwLock::new(PerimeterSnapshot::default()),
            listeners,
            tx,
        });
        // Derive immediately so the snapshot is valid before the first heartbeat
        svc.derive_and_store();
        svc
    }

    fn derive_and_store(&self) {
        let snapshot = self.derive();
        let previous_ceiling = self.snapshot.read().unwrap().ceiling;
        let new_ceiling = snapshot.ceiling;
        *self.snapshot.write().unwrap() = snapshot.clone();

        if previous_ceiling != new_ceiling {
            let _ = self.tx.send(PerimeterEvent::Shift {
                previous: previous_ceiling,
                current: new_ceiling,
            });
            if new_ceiling < previous_ceiling {
                warn!(
                    previous = ?previous_ceiling,
                    current = ?new_ceiling,
                    "Perimeter downgraded — checking for endpoints above new ceiling"
                );
            } else {
                info!(previous = ?previous_ceiling, current = ?new_ceiling, "Perimeter upgraded");
            }
        }
        let _ = self.tx.send(PerimeterEvent::Refreshed(snapshot));
    }

    fn derive(&self) -> PerimeterSnapshot {
        let public_ips = detect_public_ips();
        let has_public_ip = !public_ips.is_empty();
        let (tailscale_present, tailnet_id) = detect_tailscale();
        let private_ips = detect_private_ips();

        let mut profiles: Vec<ListenerProfile> = self
            .listeners
            .iter()
            .map(|decl| {
                let result = classify_bind_addr(decl.bind_addr, has_public_ip);
                if let Some(warning) = result.warning() {
                    warn!(purpose = decl.purpose, addr = %decl.bind_addr, "{}", warning);
                }
                ListenerProfile {
                    purpose: decl.purpose.to_string(),
                    bind_addr: decl.bind_addr,
                    port: decl.port,
                    iface: decl.iface.clone(),
                    tier: result.tier(),
                }
            })
            .collect();

        // IPC (UDS) is always Local regardless of other bindings
        profiles.push(ListenerProfile {
            purpose: "ipc".to_string(),
            bind_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            iface: None,
            tier: ExposureTier::Local,
        });

        let ceiling = profiles
            .iter()
            .map(|p| p.tier)
            .max()
            .unwrap_or(ExposureTier::Local);

        PerimeterSnapshot {
            ceiling,
            listeners: profiles,
            tailscale_present,
            tailnet_id,
            public_ips,
            private_ips,
            last_derived: unix_now(),
        }
    }
}

impl PerimeterService for HotelPerimeterService {
    fn snapshot(&self) -> PerimeterSnapshot {
        self.snapshot.read().unwrap().clone()
    }

    fn ceiling(&self) -> ExposureTier {
        self.snapshot.read().unwrap().ceiling
    }

    fn subscribe(&self) -> broadcast::Receiver<PerimeterEvent> {
        self.tx.subscribe()
    }

    fn refresh(&self) {
        self.derive_and_store();
    }
}

// ── OS probes ────────────────────────────────────────────────────────────────

/// Detect public (non-private, non-loopback, non-CGNAT) IPv4 addresses on local interfaces.
/// Uses a connect-to-8.8.8.8 trick to find the preferred outbound interface address.
fn detect_public_ips() -> Vec<IpAddr> {
    // Probe outbound preference via a non-binding connect
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
        if sock.connect("8.8.8.8:80").is_ok() {
            if let Ok(local) = sock.local_addr() {
                let ip = local.ip();
                if is_public_ip(ip) {
                    return vec![ip];
                }
            }
        }
    }
    vec![]
}

fn detect_private_ips() -> Vec<IpAddr> {
    // Same probe, return the address if it's private
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
        if sock.connect("8.8.8.8:80").is_ok() {
            if let Ok(local) = sock.local_addr() {
                let ip = local.ip();
                if !is_public_ip(ip) && !ip.is_loopback() {
                    return vec![ip];
                }
            }
        }
    }
    vec![]
}

/// Detect Tailscale presence by checking if a `100.64/10` address is reachable.
/// Returns (present, tailnet_id). Tailnet ID detection is best-effort (reads
/// from tailscale state file if present; returns None if unavailable).
fn detect_tailscale() -> (bool, Option<String>) {
    // Check for a 100.64/10 address on local interfaces via the same UDP probe trick
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
        // Connect toward a Tailscale DERP address to see if the route exists
        if sock.connect("100.64.0.1:80").is_ok() {
            if let Ok(local) = sock.local_addr() {
                if is_tailscale_addr(local.ip()) {
                    return (true, read_tailnet_id());
                }
            }
        }
    }

    // Fallback: check if tailscaled is in PATH and queryable
    if std::path::Path::new("/var/lib/tailscale/tailscaled.state").exists()
        || std::path::Path::new("/Library/Preferences/io.tailscale.ipn.macos").exists()
    {
        return (true, read_tailnet_id());
    }

    (false, None)
}

fn read_tailnet_id() -> Option<String> {
    // Best-effort: tailscale stores state that includes the tailnet name.
    // For now return None — a future slice can parse tailscale status JSON.
    None
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !v4.is_loopback()
                && !v4.is_private()
                && !v4.is_link_local()
                && !v4.is_unspecified()
                && !is_tailscale_addr(ip)
        }
        IpAddr::V6(v6) => !v6.is_loopback() && !v6.is_unspecified(),
    }
}

fn is_tailscale_addr(ip: IpAddr) -> bool {
    if let IpAddr::V4(v4) = ip {
        let o = v4.octets();
        return o[0] == 100 && (o[1] & 0xC0) == 0x40;
    }
    false
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64
}

/// Spawn a background refresh task. Fires every `interval`, re-derives the snapshot,
/// and emits PerimeterEvent::Refreshed + optionally PerimeterEvent::Shift.
pub fn spawn_refresh_loop(
    svc: Arc<HotelPerimeterService>,
    interval: Duration,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // skip the immediate first tick — already derived at construction
        loop {
            tokio::select! {
                _ = ticker.tick() => svc.refresh(),
                _ = shutdown.recv() => break,
            }
        }
    });
}
