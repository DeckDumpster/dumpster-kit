//! Direct OTLP metrics push (law-components-push-metrics-directly).
//!
//! Recording is an in-memory update under a short lock; a background task pushes cumulative
//! OTLP/HTTP protobuf to the configured endpoint on an interval with a hard deadline. A failed
//! push is dropped and counted (`dumpster_telemetry_dropped_total`), never retried inline, and
//! never touches the request path. With no endpoint configured the whole thing is a no-op.

mod layer;
mod proto;

pub use layer::{RouteMetrics, RouteMetricsLayer};

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DROPPED_METRIC: &str = "dumpster_telemetry_dropped_total";
pub const MAX_ATTRIBUTES: usize = 8;
pub const MAX_VALUE_LEN: usize = 64;
pub const MAX_SERIES: usize = 2000;
pub const DEFAULT_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[derive(Clone, Debug)]
pub struct Config {
    /// Full OTLP metrics URL, e.g. `http://victoria:8428/opentelemetry/v1/metrics`. `None` = no-op.
    pub endpoint: Option<String>,
    pub service: String,
    pub site: String,
    pub version: String,
    pub interval: Duration,
    /// Hard deadline for one push.
    pub deadline: Duration,
}

impl Config {
    pub fn new(service: &str, site: &str, version: &str, endpoint: Option<String>) -> Self {
        Config {
            endpoint: endpoint.filter(|e| !e.trim().is_empty()),
            service: service.into(),
            site: site.into(),
            version: version.into(),
            interval: Duration::from_secs(15),
            deadline: Duration::from_secs(2),
        }
    }

    /// Endpoint from `DUMPSTER_OTLP_METRICS_ENDPOINT`.
    pub fn from_env(service: &str, site: &str, version: &str) -> Self {
        Self::new(
            service,
            site,
            version,
            std::env::var("DUMPSTER_OTLP_METRICS_ENDPOINT").ok(),
        )
    }
}

type Attrs = Vec<(String, String)>;

struct Hist {
    bounds: Vec<f64>,
    description: String,
    unit: String,
    series: BTreeMap<Attrs, (Vec<u64>, u64, f64)>,
}

struct Counters {
    description: String,
    series: BTreeMap<Attrs, u64>,
}

#[derive(Default)]
struct State {
    counters: BTreeMap<String, Counters>,
    hists: BTreeMap<String, Hist>,
    dropped: BTreeMap<&'static str, u64>,
    series: usize,
}

struct Inner {
    cfg: Config,
    enabled: bool,
    start_nanos: u64,
    state: Mutex<State>,
    stop: tokio::sync::Notify,
    stopped: AtomicBool,
    client: Option<reqwest::Client>,
}

#[derive(Clone)]
pub struct Telemetry(Arc<Inner>);

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn forbidden_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    ["tenant", "email", "user", "account", "session"]
        .iter()
        .any(|f| k.contains(f))
}

/// Validates and canonicalises attributes. Err(reason) means the record is dropped.
fn clean(attrs: &[(&str, &str)]) -> Result<Attrs, &'static str> {
    if attrs.len() > MAX_ATTRIBUTES {
        return Err("too_many_attributes");
    }
    let mut out: Attrs = Vec::with_capacity(attrs.len());
    for (k, v) in attrs {
        if forbidden_key(k) || v.contains('@') {
            return Err("forbidden_attribute");
        }
        out.push(((*k).to_string(), v.chars().take(MAX_VALUE_LEN).collect()));
    }
    out.sort();
    Ok(out)
}

impl Telemetry {
    /// Builds the handle and, when an endpoint is set and a tokio runtime is running, starts the
    /// background pusher. Logs exactly one line saying which mode it is in.
    pub fn init(cfg: Config) -> Telemetry {
        let in_runtime = tokio::runtime::Handle::try_current().is_ok();
        let enabled = cfg.endpoint.is_some() && in_runtime;
        match (&cfg.endpoint, in_runtime) {
            (None, _) => {
                eprintln!("telemetry: no OTLP metrics endpoint configured; metrics disabled")
            }
            (Some(e), true) => eprintln!("telemetry: pushing OTLP metrics to {e}"),
            (Some(_), false) => eprintln!("telemetry: no tokio runtime; metrics disabled"),
        }
        let client = enabled
            .then(|| {
                reqwest::Client::builder()
                    .timeout(cfg.deadline)
                    .connect_timeout(cfg.deadline)
                    .build()
                    .ok()
            })
            .flatten();
        let enabled = enabled && client.is_some();
        let t = Telemetry(Arc::new(Inner {
            cfg,
            enabled,
            start_nanos: now_nanos(),
            state: Mutex::new(State::default()),
            stop: tokio::sync::Notify::new(),
            stopped: AtomicBool::new(false),
            client,
        }));
        if enabled {
            let inner = t.0.clone();
            tokio::spawn(async move {
                let t = Telemetry(inner);
                let mut tick = tokio::time::interval(t.0.cfg.interval);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                tick.tick().await;
                loop {
                    tokio::select! {
                        _ = tick.tick() => { t.flush().await; }
                        _ = t.0.stop.notified() => break,
                    }
                }
            });
        }
        t
    }

    /// A handle that records nothing.
    pub fn disabled() -> Telemetry {
        Telemetry::init(Config::new("none", "none", "0", None))
    }

    pub fn enabled(&self) -> bool {
        self.0.enabled
    }

    pub fn counter(&self, name: &str, description: &str) -> Counter {
        if self.0.enabled {
            self.lock()
                .counters
                .entry(name.into())
                .or_insert_with(|| Counters {
                    description: description.into(),
                    series: BTreeMap::new(),
                });
        }
        Counter {
            t: self.clone(),
            name: name.into(),
        }
    }

    pub fn histogram(
        &self,
        name: &str,
        description: &str,
        unit: &str,
        bounds: &[f64],
    ) -> Histogram {
        let mut bounds = bounds.to_vec();
        bounds.sort_by(|a, b| a.total_cmp(b));
        if self.0.enabled {
            self.lock()
                .hists
                .entry(name.into())
                .or_insert_with(|| Hist {
                    bounds,
                    description: description.into(),
                    unit: unit.into(),
                    series: BTreeMap::new(),
                });
        }
        Histogram {
            t: self.clone(),
            name: name.into(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn drop_record(st: &mut State, reason: &'static str) {
        *st.dropped.entry(reason).or_insert(0) += 1;
    }

    /// Encodes the current cumulative state; `None` when there is nothing to send.
    fn encode(&self) -> Option<Vec<u8>> {
        let now = now_nanos();
        let start = self.0.start_nanos;
        let st = self.lock();
        let mut metrics = Vec::new();
        for (name, c) in &st.counters {
            let pts: Vec<_> = c
                .series
                .iter()
                .map(|(a, v)| proto::SumPoint {
                    attributes: a,
                    start_nanos: start,
                    now_nanos: now,
                    value: *v,
                })
                .collect();
            if !pts.is_empty() {
                metrics.push(proto::sum_metric(name, &c.description, &pts));
            }
        }
        for (name, h) in &st.hists {
            let pts: Vec<_> = h
                .series
                .iter()
                .map(|(a, (b, n, s))| proto::HistPoint {
                    attributes: a,
                    start_nanos: start,
                    now_nanos: now,
                    count: *n,
                    sum: *s,
                    bucket_counts: b,
                    bounds: &h.bounds,
                })
                .collect();
            if !pts.is_empty() {
                metrics.push(proto::histogram_metric(name, &h.description, &h.unit, &pts));
            }
        }
        let dropped: Vec<(Attrs, u64)> = st
            .dropped
            .iter()
            .map(|(r, n)| (vec![("reason".to_string(), r.to_string())], *n))
            .collect();
        if !dropped.is_empty() {
            let pts: Vec<_> = dropped
                .iter()
                .map(|(a, v)| proto::SumPoint {
                    attributes: a,
                    start_nanos: start,
                    now_nanos: now,
                    value: *v,
                })
                .collect();
            metrics.push(proto::sum_metric(
                DROPPED_METRIC,
                "Telemetry records or pushes dropped",
                &pts,
            ));
        }
        if metrics.is_empty() {
            return None;
        }
        let resource = vec![
            ("service.name".to_string(), self.0.cfg.service.clone()),
            ("site".to_string(), self.0.cfg.site.clone()),
            ("service.version".to_string(), self.0.cfg.version.clone()),
        ];
        Some(proto::request(
            &resource,
            "dumpster-kit-telemetry",
            &metrics,
        ))
    }

    /// One push, bounded by the configured deadline. A failure is counted and dropped.
    pub async fn flush(&self) {
        let (Some(client), Some(url)) = (&self.0.client, &self.0.cfg.endpoint) else {
            return;
        };
        let Some(body) = self.encode() else { return };
        let ok = match client
            .post(url)
            .header("content-type", "application/x-protobuf")
            .body(body)
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        };
        if !ok {
            Self::drop_record(&mut self.lock(), "push_failed");
        }
    }

    /// Stops the pusher after one final bounded flush.
    pub async fn shutdown(&self) {
        if !self.0.stopped.swap(true, Ordering::SeqCst) {
            self.0.stop.notify_waiters();
            self.flush().await;
        }
    }
}

#[derive(Clone)]
pub struct Counter {
    t: Telemetry,
    name: String,
}

impl Counter {
    pub fn add(&self, n: u64, attrs: &[(&str, &str)]) {
        if !self.t.0.enabled {
            return;
        }
        let mut st = self.t.lock();
        let attrs = match clean(attrs) {
            Ok(a) => a,
            Err(r) => return Telemetry::drop_record(&mut st, r),
        };
        let st = &mut *st;
        let Some(c) = st.counters.get_mut(&self.name) else {
            return;
        };
        if !c.series.contains_key(&attrs) {
            if st.series >= MAX_SERIES {
                return Telemetry::drop_record(st, "series_cap");
            }
            st.series += 1;
        }
        *c.series.entry(attrs).or_insert(0) += n;
    }
}

#[derive(Clone)]
pub struct Histogram {
    t: Telemetry,
    name: String,
}

impl Histogram {
    pub fn record(&self, value: f64, attrs: &[(&str, &str)]) {
        if !self.t.0.enabled {
            return;
        }
        let mut st = self.t.lock();
        let attrs = match clean(attrs) {
            Ok(a) => a,
            Err(r) => return Telemetry::drop_record(&mut st, r),
        };
        let st = &mut *st;
        let Some(h) = st.hists.get_mut(&self.name) else {
            return;
        };
        if !h.series.contains_key(&attrs) {
            if st.series >= MAX_SERIES {
                return Telemetry::drop_record(st, "series_cap");
            }
            st.series += 1;
        }
        let idx = h.bounds.partition_point(|b| *b < value);
        let n = h.bounds.len() + 1;
        let e = h
            .series
            .entry(attrs)
            .or_insert_with(|| (vec![0; n], 0, 0.0));
        e.0[idx] += 1;
        e.1 += 1;
        e.2 += value;
    }
}
