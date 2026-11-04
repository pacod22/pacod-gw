//! The bridge's own figures, in the Prometheus text format: they also measure demand for the
//! broker integration (docs/pacod-gw.md in pacod-broker, P0).

use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct Metrics {
    /// The first refresh went through.
    pub ready: AtomicBool,
    pub offerings: AtomicU64,
    pub announces: AtomicU64,
    pub heartbeat_failures: AtomicU64,
    pub reports_sent: AtomicU64,
    pub reports_counted: AtomicU64,
    pub tickets_seen: AtomicU64,
    pub client_reports: AtomicU64,
    pub failures: AtomicU64,
}

impl Metrics {
    pub fn render(&self) -> String {
        let mut s = String::new();
        let rows: [(&str, &str, &str, &AtomicU64); 8] = [
            ("gwb_offerings", "gauge", "Templates offered on the broker", &self.offerings),
            ("gwb_announces_total", "counter", "Announcements and updates the broker accepted", &self.announces),
            (
                "gwb_heartbeat_failures_total",
                "counter",
                "Heartbeats or re-announcements that failed",
                &self.heartbeat_failures,
            ),
            ("gwb_reports_sent_total", "counter", "Usage reports sent", &self.reports_sent),
            ("gwb_reports_counted_total", "counter", "Usage reports the broker counted", &self.reports_counted),
            ("gwb_tickets_seen_total", "counter", "Calls to offerings that quoted a broker ticket", &self.tickets_seen),
            ("gwb_client_reports_total", "counter", "Reports about clients sent", &self.client_reports),
            ("gwb_failures_total", "counter", "Rounds that failed (refresh, live state, evidence)", &self.failures),
        ];
        for (name, kind, help, v) in rows {
            let _ = writeln!(s, "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {}", v.load(Ordering::Relaxed));
        }
        s
    }
}
