//! Hand-encoded OTLP/HTTP protobuf (`ExportMetricsServiceRequest`). Only the handful of fields
//! the push uses; VictoriaMetrics ingests this at `/opentelemetry/v1/metrics`.

pub(crate) fn varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

fn tag(buf: &mut Vec<u8>, field: u32, wire: u8) {
    varint(buf, ((field as u64) << 3) | wire as u64);
}

pub(crate) fn bytes_field(buf: &mut Vec<u8>, field: u32, data: &[u8]) {
    tag(buf, field, 2);
    varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

fn varint_field(buf: &mut Vec<u8>, field: u32, v: u64) {
    tag(buf, field, 0);
    varint(buf, v);
}

fn fixed64_field(buf: &mut Vec<u8>, field: u32, v: u64) {
    tag(buf, field, 1);
    buf.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn key_value(key: &str, value: &str) -> Vec<u8> {
    let mut any = Vec::new();
    bytes_field(&mut any, 1, value.as_bytes());
    let mut kv = Vec::new();
    bytes_field(&mut kv, 1, key.as_bytes());
    bytes_field(&mut kv, 2, &any);
    kv
}

fn attrs(buf: &mut Vec<u8>, field: u32, attributes: &[(String, String)]) {
    for (k, v) in attributes {
        bytes_field(buf, field, &key_value(k, v));
    }
}

const CUMULATIVE: u64 = 2;

pub(crate) struct SumPoint<'a> {
    pub attributes: &'a [(String, String)],
    pub start_nanos: u64,
    pub now_nanos: u64,
    pub value: u64,
}

pub(crate) struct HistPoint<'a> {
    pub attributes: &'a [(String, String)],
    pub start_nanos: u64,
    pub now_nanos: u64,
    pub count: u64,
    pub sum: f64,
    pub bucket_counts: &'a [u64],
    pub bounds: &'a [f64],
}

pub(crate) fn sum_metric(name: &str, description: &str, points: &[SumPoint]) -> Vec<u8> {
    let mut sum = Vec::new();
    for p in points {
        let mut dp = Vec::new();
        fixed64_field(&mut dp, 2, p.start_nanos);
        fixed64_field(&mut dp, 3, p.now_nanos);
        fixed64_field(&mut dp, 6, p.value);
        attrs(&mut dp, 7, p.attributes);
        bytes_field(&mut sum, 1, &dp);
    }
    varint_field(&mut sum, 2, CUMULATIVE);
    varint_field(&mut sum, 3, 1);
    let mut m = Vec::new();
    bytes_field(&mut m, 1, name.as_bytes());
    bytes_field(&mut m, 2, description.as_bytes());
    bytes_field(&mut m, 7, &sum);
    m
}

pub(crate) fn histogram_metric(
    name: &str,
    description: &str,
    unit: &str,
    points: &[HistPoint],
) -> Vec<u8> {
    let mut h = Vec::new();
    for p in points {
        let mut dp = Vec::new();
        fixed64_field(&mut dp, 2, p.start_nanos);
        fixed64_field(&mut dp, 3, p.now_nanos);
        fixed64_field(&mut dp, 4, p.count);
        fixed64_field(&mut dp, 5, p.sum.to_bits());
        let mut counts = Vec::new();
        for c in p.bucket_counts {
            counts.extend_from_slice(&c.to_le_bytes());
        }
        bytes_field(&mut dp, 6, &counts);
        let mut bounds = Vec::new();
        for b in p.bounds {
            bounds.extend_from_slice(&b.to_le_bytes());
        }
        bytes_field(&mut dp, 7, &bounds);
        attrs(&mut dp, 9, p.attributes);
        bytes_field(&mut h, 1, &dp);
    }
    varint_field(&mut h, 2, CUMULATIVE);
    let mut m = Vec::new();
    bytes_field(&mut m, 1, name.as_bytes());
    bytes_field(&mut m, 2, description.as_bytes());
    bytes_field(&mut m, 3, unit.as_bytes());
    bytes_field(&mut m, 9, &h);
    m
}

pub(crate) fn request(resource: &[(String, String)], scope: &str, metrics: &[Vec<u8>]) -> Vec<u8> {
    let mut res = Vec::new();
    attrs(&mut res, 1, resource);
    let mut sc = Vec::new();
    bytes_field(&mut sc, 1, scope.as_bytes());
    let mut sm = Vec::new();
    bytes_field(&mut sm, 1, &sc);
    for m in metrics {
        bytes_field(&mut sm, 2, m);
    }
    let mut rm = Vec::new();
    bytes_field(&mut rm, 1, &res);
    bytes_field(&mut rm, 2, &sm);
    let mut out = Vec::new();
    bytes_field(&mut out, 1, &rm);
    out
}
