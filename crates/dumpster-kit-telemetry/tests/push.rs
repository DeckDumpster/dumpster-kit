use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Router, routing::get};
use dumpster_kit_telemetry::{Config, RouteMetricsLayer, Telemetry};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower::ServiceExt;

// ---- minimal protobuf reader -------------------------------------------------------------
#[derive(Debug, Clone)]
enum V {
    Num(u64),
    Bytes(Vec<u8>),
}

fn parse(mut b: &[u8]) -> Vec<(u32, V)> {
    let mut out = vec![];
    while !b.is_empty() {
        let (t, n) = rv(b);
        b = &b[n..];
        let f = (t >> 3) as u32;
        match t & 7 {
            0 => {
                let (v, n) = rv(b);
                b = &b[n..];
                out.push((f, V::Num(v)));
            }
            1 => {
                out.push((f, V::Num(u64::from_le_bytes(b[..8].try_into().unwrap()))));
                b = &b[8..];
            }
            2 => {
                let (l, n) = rv(b);
                b = &b[n..];
                out.push((f, V::Bytes(b[..l as usize].to_vec())));
                b = &b[l as usize..];
            }
            w => panic!("wire {w}"),
        }
    }
    out
}
fn rv(b: &[u8]) -> (u64, usize) {
    let (mut v, mut s) = (0u64, 0);
    for (i, x) in b.iter().enumerate() {
        v |= ((x & 0x7f) as u64) << s;
        s += 7;
        if x & 0x80 == 0 {
            return (v, i + 1);
        }
    }
    panic!()
}
fn sub(m: &[(u32, V)], f: u32) -> Vec<Vec<(u32, V)>> {
    m.iter()
        .filter_map(|(k, v)| match v {
            V::Bytes(b) if *k == f => Some(parse(b)),
            _ => None,
        })
        .collect()
}
fn s(m: &[(u32, V)], f: u32) -> String {
    m.iter()
        .find_map(|(k, v)| match v {
            V::Bytes(b) if *k == f => Some(String::from_utf8(b.clone()).unwrap()),
            _ => None,
        })
        .unwrap_or_default()
}
fn n(m: &[(u32, V)], f: u32) -> u64 {
    m.iter()
        .find_map(|(k, v)| match v {
            V::Num(x) if *k == f => Some(*x),
            _ => None,
        })
        .unwrap_or(0)
}
fn kvs(m: &[(u32, V)], f: u32) -> Vec<(String, String)> {
    sub(m, f)
        .iter()
        .map(|kv| (s(kv, 1), s(&sub(kv, 2)[0], 1)))
        .collect()
}

struct Decoded {
    resource: Vec<(String, String)>,
    metrics: Vec<Vec<(u32, V)>>,
}
fn decode(body: &[u8]) -> Decoded {
    let req = parse(body);
    let rm = &sub(&req, 1)[0];
    let resource = kvs(&sub(rm, 1)[0], 1);
    let metrics = sub(&sub(rm, 2)[0], 2);
    Decoded { resource, metrics }
}

async fn fake_endpoint() -> (String, Arc<Mutex<Vec<Vec<u8>>>>) {
    let got = Arc::new(Mutex::new(vec![]));
    let g = got.clone();
    let app = Router::new().route(
        "/opentelemetry/v1/metrics",
        axum::routing::post(move |body: axum::body::Bytes| {
            let g = g.clone();
            async move {
                g.lock().unwrap().push(body.to_vec());
                StatusCode::OK
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/opentelemetry/v1/metrics",
        l.local_addr().unwrap()
    );
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, got)
}

fn app(t: &Telemetry) -> Router {
    Router::new()
        .route("/card/{id}", get(|| async { "ok" }))
        .route("/boom", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .layer(RouteMetricsLayer::new(t))
}

fn get_req(p: &str) -> Request<Body> {
    Request::get(p).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn pushes_route_metrics_with_template_buckets_and_resource() {
    let (url, got) = fake_endpoint().await;
    let t = Telemetry::init(Config::new(
        "pokedumpster",
        "pokedumpster.com",
        "1.2.3",
        Some(url),
    ));
    let app = app(&t);
    for id in ["a1", "b2", "c3"] {
        app.clone()
            .oneshot(get_req(&format!("/card/{id}")))
            .await
            .unwrap();
    }
    app.clone().oneshot(get_req("/boom")).await.unwrap();
    app.clone()
        .oneshot(get_req("/nope/secret-path"))
        .await
        .unwrap();
    t.flush().await;

    let bodies = got.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1);
    let d = decode(&bodies[0]);
    assert!(
        d.resource
            .contains(&("service.name".into(), "pokedumpster".into()))
    );
    assert!(
        d.resource
            .contains(&("site".into(), "pokedumpster.com".into()))
    );
    assert!(
        d.resource
            .contains(&("service.version".into(), "1.2.3".into()))
    );

    let by_name = |name: &str| d.metrics.iter().find(|m| s(m, 1) == name).unwrap().clone();

    let sum = sub(&by_name("http_server_requests_total"), 7).remove(0);
    assert_eq!(n(&sum, 2), 2, "cumulative");
    let mut seen = vec![];
    for dp in sub(&sum, 1) {
        let a = kvs(&dp, 7);
        seen.push((a.clone(), n(&dp, 6)));
    }
    let find = |route: &str, class: &str| {
        seen.iter()
            .find(|(a, _)| {
                a.contains(&("route".into(), route.into()))
                    && a.contains(&("status_class".into(), class.into()))
            })
            .map(|(_, v)| *v)
    };
    assert_eq!(find("/card/{id}", "2xx"), Some(3));
    assert_eq!(find("/boom", "5xx"), Some(1));
    assert_eq!(find("unmatched", "4xx"), Some(1));
    let all = format!("{seen:?}");
    assert!(
        !all.contains("a1") && !all.contains("secret-path"),
        "raw path leaked: {all}"
    );

    let hm = by_name("http_server_request_duration_seconds");
    assert_eq!(s(&hm, 3), "s");
    let h = sub(&hm, 9).remove(0);
    for dp in sub(&h, 1) {
        let bounds: Vec<f64> = match &dp.iter().find(|(k, _)| *k == 7).unwrap().1 {
            V::Bytes(b) => b
                .chunks(8)
                .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
                .collect(),
            _ => panic!(),
        };
        assert_eq!(bounds, dumpster_kit_telemetry::DEFAULT_BUCKETS);
        let counts: Vec<u64> = match &dp.iter().find(|(k, _)| *k == 6).unwrap().1 {
            V::Bytes(b) => b
                .chunks(8)
                .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                .collect(),
            _ => panic!(),
        };
        assert_eq!(counts.len(), bounds.len() + 1);
        assert_eq!(counts.iter().sum::<u64>(), n(&dp, 4));
    }
}

#[tokio::test]
async fn custom_metrics_and_forbidden_attributes() {
    let (url, got) = fake_endpoint().await;
    let t = Telemetry::init(Config::new("svc", "site", "1", Some(url)));
    let c = t.counter("search_total", "searches");
    let h = t.histogram("search_depth", "paging depth", "1", &[1.0, 10.0]);
    c.add(2, &[("kind", "card")]);
    c.add(1, &[("tenant_id", "t-123")]);
    c.add(1, &[("who", "ryan@example.com")]);
    c.add(1, &[("Email", "x")]);
    h.record(5.0, &[("kind", "card")]);
    t.flush().await;
    let d = decode(&got.lock().unwrap()[0]);
    let names: Vec<String> = d.metrics.iter().map(|m| s(m, 1)).collect();
    assert!(names.contains(&"search_total".to_string()));
    assert!(names.contains(&"search_depth".to_string()));
    assert!(names.contains(&"dumpster_telemetry_dropped_total".to_string()));
    let raw = String::from_utf8_lossy(&got.lock().unwrap()[0]).to_string();
    for bad in ["tenant", "t-123", "example.com", "Email"] {
        assert!(!raw.contains(bad), "{bad} leaked");
    }
    let dropped = d
        .metrics
        .iter()
        .find(|m| s(m, 1) == "dumpster_telemetry_dropped_total")
        .unwrap();
    let total: u64 = sub(&sub(dropped, 7)[0], 1).iter().map(|dp| n(dp, 6)).sum();
    assert_eq!(total, 3);
}

#[tokio::test]
async fn blackholed_endpoint_adds_no_request_latency() {
    // Accepts nothing: connections hang until the push deadline.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/opentelemetry/v1/metrics",
        l.local_addr().unwrap()
    );
    let mut cfg = Config::new("svc", "site", "1", Some(url));
    cfg.interval = Duration::from_millis(20);
    cfg.deadline = Duration::from_millis(500);
    let t = Telemetry::init(cfg);
    let app = app(&t);
    app.clone().oneshot(get_req("/card/x")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await; // pushes now in flight and hanging
    let start = Instant::now();
    for _ in 0..200 {
        app.clone().oneshot(get_req("/card/x")).await.unwrap();
    }
    let per = start.elapsed() / 200;
    assert!(per < Duration::from_millis(5), "per-request {per:?}");
    drop(l);
}

#[tokio::test]
async fn failed_push_is_counted_not_fatal() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/x", l.local_addr().unwrap());
    drop(l); // refused
    let (good, got) = fake_endpoint().await;
    let t = Telemetry::init(Config::new("svc", "site", "1", Some(url)));
    t.counter("c", "c").add(1, &[]);
    t.flush().await;
    assert!(got.lock().unwrap().is_empty());
    assert!(t.enabled());
    let _ = good;
}

#[tokio::test]
async fn unset_endpoint_is_a_noop() {
    let t = Telemetry::init(Config::new("svc", "site", "1", None));
    assert!(!t.enabled());
    t.counter("c", "c").add(1, &[]);
    t.flush().await;
    let r = app(&t).oneshot(get_req("/card/1")).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}
