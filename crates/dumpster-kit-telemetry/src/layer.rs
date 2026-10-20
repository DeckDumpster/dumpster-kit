use crate::{Counter, DEFAULT_BUCKETS, Histogram, Telemetry};
use axum::extract::{MatchedPath, Request};
use axum::response::Response;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;
use tower::{Layer, Service};

pub const REQUESTS_METRIC: &str = "http_server_requests_total";
pub const DURATION_METRIC: &str = "http_server_request_duration_seconds";

/// Per-route request count and duration, labelled by route template and status class.
/// Add with `router.layer(RouteMetricsLayer::new(&telemetry))` so the route is matched first.
#[derive(Clone)]
pub struct RouteMetricsLayer(RouteMetrics);

#[derive(Clone)]
pub struct RouteMetrics {
    requests: Counter,
    duration: Histogram,
}

impl RouteMetricsLayer {
    pub fn new(t: &Telemetry) -> Self {
        RouteMetricsLayer(RouteMetrics {
            requests: t.counter(
                REQUESTS_METRIC,
                "HTTP requests by route template and status class",
            ),
            duration: t.histogram(
                DURATION_METRIC,
                "HTTP request duration by route template and status class",
                "s",
                DEFAULT_BUCKETS,
            ),
        })
    }
}

impl<S> Layer<S> for RouteMetricsLayer {
    type Service = RouteMetricsService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RouteMetricsService {
            inner,
            m: self.0.clone(),
        }
    }
}

#[derive(Clone)]
pub struct RouteMetricsService<S> {
    inner: S,
    m: RouteMetrics,
}

fn class(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

impl<S> Service<Request> for RouteMetricsService<S>
where
    S: Service<Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        // Never the raw path: an unmatched request is one bounded label.
        let route = req
            .extensions()
            .get::<MatchedPath>()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "unmatched".to_string());
        let method = req.method().as_str().to_string();
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let m = self.m.clone();
        let start = Instant::now();
        let fut = inner.call(req);
        Box::pin(async move {
            let res = fut.await?;
            let attrs = [
                ("method", method.as_str()),
                ("route", route.as_str()),
                ("status_class", class(res.status().as_u16())),
            ];
            m.requests.add(1, &attrs);
            m.duration.record(start.elapsed().as_secs_f64(), &attrs);
            Ok(res)
        })
    }
}
