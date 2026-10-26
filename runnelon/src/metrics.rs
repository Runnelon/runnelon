//! Request counters and a fixed-bucket latency histogram.
//!
//! Metrics are lock-free (atomics) and rendered in Prometheus text format by the
//! management listener. Bucket bounds are milliseconds; recording increments
//! every bucket whose bound the observation does not exceed, which makes the
//! exported buckets cumulative as Prometheus expects.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Upper bounds (ms) of the latency histogram; `+Inf` is implied.
const BUCKETS_MS: [u64; 12] = [1, 5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000];

const STATUS_CLASSES: usize = 5;

/// Process-wide request metrics.
#[derive(Debug, Default)]
pub struct Metrics {
    requests: AtomicU64,
    inflight: AtomicU64,
    statuses: [AtomicU64; STATUS_CLASSES],
    buckets: [AtomicU64; BUCKETS_MS.len()],
    duration_sum_ms: AtomicU64,
    duration_count: AtomicU64,
}

impl Metrics {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a proxied request started.
    pub fn request_started(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.inflight.fetch_add(1, Ordering::Relaxed);
    }

    /// Record completion of a proxied request.
    pub fn request_finished(&self, status: Option<u16>, elapsed: Duration) {
        self.inflight.fetch_sub(1, Ordering::Relaxed);

        if let Some(status) = status {
            if (100..600).contains(&status) {
                self.statuses[usize::from(status / 100 - 1)].fetch_add(1, Ordering::Relaxed);
            }
        }

        let millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        self.duration_sum_ms.fetch_add(millis, Ordering::Relaxed);
        self.duration_count.fetch_add(1, Ordering::Relaxed);
        for (index, bound) in BUCKETS_MS.iter().enumerate() {
            if millis <= *bound {
                self.buckets[index].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Render all metrics in Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(2048);

        out.push_str("# HELP runnelon_requests_total Proxied requests started.\n");
        out.push_str("# TYPE runnelon_requests_total counter\n");
        let _ = writeln!(
            out,
            "runnelon_requests_total {}",
            self.requests.load(Ordering::Relaxed)
        );

        out.push_str("# HELP runnelon_inflight_requests Requests currently being proxied.\n");
        out.push_str("# TYPE runnelon_inflight_requests gauge\n");
        let _ = writeln!(
            out,
            "runnelon_inflight_requests {}",
            self.inflight.load(Ordering::Relaxed)
        );

        out.push_str("# HELP runnelon_responses_total Responses by status class.\n");
        out.push_str("# TYPE runnelon_responses_total counter\n");
        for (index, count) in self.statuses.iter().enumerate() {
            let _ = writeln!(
                out,
                "runnelon_responses_total{{class=\"{}\"}} {}",
                index + 1,
                count.load(Ordering::Relaxed)
            );
        }

        out.push_str("# HELP runnelon_response_duration_ms Time to complete proxied requests in milliseconds.\n");
        out.push_str("# TYPE runnelon_response_duration_ms histogram\n");
        for (index, bound) in BUCKETS_MS.iter().enumerate() {
            let _ = writeln!(
                out,
                "runnelon_response_duration_ms_bucket{{le=\"{bound}\"}} {}",
                self.buckets[index].load(Ordering::Relaxed)
            );
        }
        let count = self.duration_count.load(Ordering::Relaxed);
        let _ = writeln!(
            out,
            "runnelon_response_duration_ms_bucket{{le=\"+Inf\"}} {count}"
        );
        let _ = writeln!(
            out,
            "runnelon_response_duration_ms_sum {}",
            self.duration_sum_ms.load(Ordering::Relaxed)
        );
        let _ = writeln!(out, "runnelon_response_duration_ms_count {count}");

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_counters_and_cumulative_buckets() {
        let metrics = Metrics::new();
        metrics.request_started();
        metrics.request_finished(Some(200), Duration::from_millis(7));
        metrics.request_started();
        metrics.request_finished(Some(404), Duration::from_millis(30));
        metrics.request_started();
        metrics.request_finished(Some(502), Duration::from_millis(2_000));

        let text = metrics.render();
        assert!(text.contains("runnelon_requests_total 3"), "{text}");
        assert!(text.contains("runnelon_inflight_requests 0"), "{text}");
        assert!(
            text.contains("runnelon_responses_total{class=\"2\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("runnelon_responses_total{class=\"4\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("runnelon_responses_total{class=\"5\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("runnelon_responses_total{class=\"1\"} 0"),
            "{text}"
        );

        // 7 ms counts in le=10 and above; 30 ms in le=50 and above; 2 s in le=2500+.
        assert!(
            text.contains("runnelon_response_duration_ms_bucket{le=\"5\"} 0"),
            "{text}"
        );
        assert!(
            text.contains("runnelon_response_duration_ms_bucket{le=\"10\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("runnelon_response_duration_ms_bucket{le=\"50\"} 2"),
            "{text}"
        );
        assert!(
            text.contains("runnelon_response_duration_ms_bucket{le=\"1000\"} 2"),
            "{text}"
        );
        assert!(
            text.contains("runnelon_response_duration_ms_bucket{le=\"2500\"} 3"),
            "{text}"
        );
        assert!(
            text.contains("runnelon_response_duration_ms_bucket{le=\"+Inf\"} 3"),
            "{text}"
        );
        assert!(
            text.contains("runnelon_response_duration_ms_count 3"),
            "{text}"
        );
        assert!(
            text.contains("runnelon_response_duration_ms_sum 2037"),
            "{text}"
        );
    }
}
