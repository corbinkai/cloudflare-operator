use std::sync::LazyLock;

use prometheus::{
    CounterVec, HistogramVec, TextEncoder, register_counter_vec, register_histogram_vec,
};

pub static RECONCILE_TOTAL: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "controller_reconcile_total",
        "Total number of reconciliations",
        &["controller", "result"]
    )
    .unwrap()
});

pub static RECONCILE_DURATION: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "controller_reconcile_duration_seconds",
        "Duration of reconciliations in seconds",
        &["controller"]
    )
    .unwrap()
});

pub static CF_API_REQUESTS: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        "cloudflare_api_requests_total",
        "Total Cloudflare API requests",
        &["method", "status"]
    )
    .unwrap()
});

pub static CF_API_DURATION: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "cloudflare_api_request_duration_seconds",
        "Duration of Cloudflare API requests in seconds",
        &["method"]
    )
    .unwrap()
});

pub fn gather_metrics() -> String {
    let encoder = TextEncoder::new();
    let mut buffer = String::new();
    let metrics = prometheus::gather();
    encoder.encode_utf8(&metrics, &mut buffer).unwrap();
    buffer
}
