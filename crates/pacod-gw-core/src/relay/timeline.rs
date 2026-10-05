//! The timing points of a call and the stages computed from them (concept section 14.2).

use std::time::{Duration, Instant, SystemTime};

use pacod_gw_events::Timing;

#[derive(Debug, Clone)]
pub struct Timeline {
    /// The handler was entered.
    pub start: Instant,
    pub wall_start: SystemTime,
    /// Reading the request body began. What the gateway did before that (authentication,
    /// looking up the provider) is its own work and counts towards `pre_ms`.
    body_begin: Option<Instant>,
    /// The request body was complete.
    body_done: Option<Instant>,
    /// The first provider attempt started.
    first_attempt: Option<Instant>,
    /// The attempt that produced the response started.
    final_attempt: Option<Instant>,
    /// A connection to the provider was ready.
    connected: Option<Instant>,
    /// The provider's response head arrived.
    upstream_head: Option<Instant>,
    /// Our response head was handed to the client connection.
    client_head: Option<Instant>,
    /// The provider's last byte arrived.
    upstream_last: Option<Instant>,
    /// Our last byte was handed to the client connection.
    client_last: Option<Instant>,
    /// The longest time a chunk spent in the gateway.
    relay_max: Duration,
    /// The response was held completely before it was sent (translation of a non-streamed
    /// answer), so the gateway's work on it lies after the provider's last byte.
    buffered: bool,
}

fn ms(d: Duration) -> f32 {
    (d.as_secs_f64() * 1000.0) as f32
}

fn between(from: Option<Instant>, to: Option<Instant>) -> Duration {
    match (from, to) {
        (Some(a), Some(b)) => b.saturating_duration_since(a),
        _ => Duration::ZERO,
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Timeline {
    pub fn new() -> Self {
        Self::starting_at(Instant::now())
    }

    pub fn starting_at(start: Instant) -> Self {
        Self {
            start,
            wall_start: SystemTime::now(),
            body_begin: None,
            body_done: None,
            first_attempt: None,
            final_attempt: None,
            connected: None,
            upstream_head: None,
            client_head: None,
            upstream_last: None,
            client_last: None,
            relay_max: Duration::ZERO,
            buffered: false,
        }
    }

    /// Call right before the request body is read.
    pub fn body_begin(&mut self) {
        self.body_begin = Some(Instant::now());
    }

    /// The gateway's own work before it started reading the body.
    fn early(&self) -> Duration {
        between(Some(self.start), self.body_begin)
    }

    pub fn body_done(&mut self) {
        self.body_done = Some(Instant::now());
    }

    /// Call before every provider attempt.
    pub fn attempt(&mut self) {
        let now = Instant::now();
        self.first_attempt.get_or_insert(now);
        self.final_attempt = Some(now);
        self.connected = None;
    }

    pub fn connected(&mut self, at: Option<Instant>) {
        self.connected = at;
    }

    pub fn upstream_head(&mut self, at: Instant) {
        self.upstream_head = Some(at);
    }

    pub fn client_head(&mut self) {
        self.client_head = Some(Instant::now());
    }

    pub fn upstream_chunk(&mut self, at: Instant) {
        self.upstream_last = Some(at);
    }

    pub fn client_chunk(&mut self, at: Instant) {
        self.client_last = Some(at);
    }

    pub fn chunk_spent(&mut self, d: Duration) {
        self.relay_max = self.relay_max.max(d);
    }

    pub fn set_buffered(&mut self) {
        self.buffered = true;
    }

    /// The gateway delay as known when the response head is written: the processing before
    /// the provider call plus the processing since the provider's head arrived.
    pub fn delay_at_head_ms(&self) -> f32 {
        let now = Instant::now();
        let pre = self.early() + between(self.body_done.or(Some(self.start)), self.first_attempt.or(Some(now)));
        // A response that was held completely is delayed from the provider's last byte on.
        let from = if self.buffered { self.upstream_last.or(self.upstream_head) } else { self.upstream_head };
        let post = from.map_or(Duration::ZERO, |h| now.saturating_duration_since(h));
        ms(pre + post)
    }

    /// The stages as far as they are known while the response is being written, for the
    /// interfaces that report them in the response itself.
    pub fn so_far(&self) -> serde_json::Value {
        let round = |v: f32| (f64::from(v) * 1000.0).round() / 1000.0;
        let t = self.timing(Instant::now());
        let delay = self.delay_at_head_ms();
        serde_json::json!({
            "gateway_delay": round(delay),
            "pre": round(t.pre_ms),
            "post": round((delay - t.pre_ms).max(0.0)),
            "connect": round(t.connect_ms),
            "retry": round(t.retry_ms),
            "upstream_first_byte": round(t.upstream_first_byte_ms),
            "total": round(t.total_ms),
        })
    }

    /// The stages, computed when the call is settled.
    pub fn timing(&self, settled: Instant) -> Timing {
        let body_done = self.body_done.unwrap_or(self.start);
        // A call that never reached a provider spent all its time in the gateway.
        let end = self.client_last.or(self.client_head).unwrap_or(settled);
        let pre = self.early()
            + match self.first_attempt {
                Some(a) => a.saturating_duration_since(body_done),
                None => end.saturating_duration_since(body_done),
            };
        let post = if self.buffered {
            between(self.upstream_last.or(self.upstream_head), self.client_last.or(self.client_head))
        } else {
            between(self.upstream_head, self.client_head)
        };
        let sent_from = self.connected.or(self.final_attempt);
        let connect = between(self.final_attempt, self.connected);
        Timing {
            gateway_delay_ms: ms(pre + post),
            receive_ms: ms(body_done.saturating_duration_since(self.body_begin.unwrap_or(self.start))),
            pre_ms: ms(pre),
            connect_ms: ms(connect),
            retry_ms: ms(between(self.first_attempt, self.final_attempt)),
            upstream_first_byte_ms: ms(between(sent_from, self.upstream_head)),
            upstream_stream_ms: ms(between(self.upstream_head, self.upstream_last)),
            post_ms: ms(post),
            relay_max_ms: ms(self.relay_max),
            settle_ms: ms(settled.saturating_duration_since(end)),
            total_ms: ms(end.saturating_duration_since(self.start)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_before_the_body_is_read_is_the_gateways_not_the_clients() {
        let t0 = Instant::now();
        let at = |n: u64| Some(t0 + Duration::from_millis(n));
        let mut t = Timeline::starting_at(t0);
        // 2 ms of authentication, 10 ms waiting for a slow upload, 1 ms until the provider call.
        t.body_begin = at(2);
        t.body_done = at(12);
        t.first_attempt = at(13);
        t.final_attempt = at(13);
        t.upstream_head = at(113);
        t.client_head = at(113);
        t.client_last = at(113);
        let s = t.timing(t0 + Duration::from_millis(114));
        assert!((s.receive_ms - 10.0).abs() < 0.01, "{}", s.receive_ms);
        assert!((s.pre_ms - 3.0).abs() < 0.01, "{}", s.pre_ms);
        assert!((s.gateway_delay_ms - 3.0).abs() < 0.01);
    }

    /// A timeline with every point set at a fixed number of milliseconds after the start.
    #[allow(clippy::too_many_arguments)]
    fn at(
        body: u64,
        first: u64,
        fin: u64,
        conn: u64,
        up_head: u64,
        cl_head: u64,
        up_last: u64,
        cl_last: u64,
    ) -> (Timeline, Instant) {
        let t0 = Instant::now();
        let p = |n: u64| Some(t0 + Duration::from_millis(n));
        let mut t = Timeline::starting_at(t0);
        t.body_done = p(body);
        t.first_attempt = p(first);
        t.final_attempt = p(fin);
        t.connected = p(conn);
        t.upstream_head = p(up_head);
        t.client_head = p(cl_head);
        t.upstream_last = p(up_last);
        t.client_last = p(cl_last);
        (t, t0)
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn streamed_call_stages() {
        // 2 ms receiving, 1 ms before the call, a retry costing 30 ms, 5 ms to connect,
        // 600 ms to the provider's first byte, 1 ms until ours, a 4 s stream.
        let (t, t0) = at(2, 3, 33, 38, 638, 639, 4638, 4638);
        let s = t.timing(t0 + Duration::from_millis(4640));
        assert!(close(s.receive_ms, 2.0));
        assert!(close(s.pre_ms, 1.0));
        assert!(close(s.retry_ms, 30.0));
        assert!(close(s.connect_ms, 5.0));
        assert!(close(s.upstream_first_byte_ms, 600.0));
        assert!(close(s.post_ms, 1.0));
        assert!(close(s.upstream_stream_ms, 4000.0));
        assert!(close(s.gateway_delay_ms, 2.0), "pre + post, without connect and retry");
        assert!(close(s.settle_ms, 2.0));
        assert!(close(s.total_ms, 4638.0));
    }

    #[test]
    fn buffered_call_counts_the_work_after_the_last_byte() {
        let (mut t, t0) = at(1, 2, 2, 2, 502, 0, 520, 523);
        t.client_head = None;
        t.set_buffered();
        let s = t.timing(t0 + Duration::from_millis(523));
        assert!(close(s.pre_ms, 1.0));
        assert!(close(s.post_ms, 3.0), "from the provider's last byte to ours");
        assert!(close(s.gateway_delay_ms, 4.0));
        assert!(close(s.upstream_stream_ms, 18.0));
    }

    #[test]
    fn a_call_rejected_before_any_provider_attempt() {
        let t0 = Instant::now();
        let mut t = Timeline::starting_at(t0);
        t.body_done = Some(t0 + Duration::from_millis(1));
        let s = t.timing(t0 + Duration::from_micros(1400));
        assert!(close(s.pre_ms, 0.4));
        assert!(close(s.gateway_delay_ms, 0.4));
        assert_eq!(s.upstream_first_byte_ms, 0.0);
        assert_eq!(s.connect_ms, 0.0);
    }

    #[test]
    fn a_reused_connection_has_no_connect_time() {
        let (t, t0) = at(0, 1, 1, 1, 301, 301, 301, 301);
        let s = t.timing(t0 + Duration::from_millis(301));
        assert_eq!(s.connect_ms, 0.0);
        assert!(close(s.upstream_first_byte_ms, 300.0));
    }

    #[test]
    fn relay_max_keeps_the_largest_chunk_time() {
        let mut t = Timeline::new();
        t.chunk_spent(Duration::from_micros(40));
        t.chunk_spent(Duration::from_micros(900));
        t.chunk_spent(Duration::from_micros(10));
        assert!(close(t.timing(Instant::now()).relay_max_ms, 0.9));
    }
}
