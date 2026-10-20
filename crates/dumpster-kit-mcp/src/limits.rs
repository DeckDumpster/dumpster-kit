use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// `tools/call`s one grant may make per `window`.
    pub calls_per_window: u32,
    pub window: Duration,
    /// A result whose serialized size exceeds this is refused, never truncated.
    pub max_result_bytes: usize,
    pub max_request_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            calls_per_window: 120,
            window: Duration::from_secs(60),
            max_result_bytes: 256 * 1024,
            max_request_bytes: 256 * 1024,
        }
    }
}

/// Fixed-window counter per grant.
pub struct RateLimiter {
    limit: u32,
    window: Duration,
    state: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    pub fn new(limits: &Limits) -> Self {
        Self {
            limit: limits.calls_per_window,
            window: limits.window,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// `Ok` if the call is allowed, else the time until the grant's window reopens.
    pub fn check(&self, grant: &str) -> Result<(), Duration> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.retain(|_, (start, _)| now.duration_since(*start) < self.window);
        let entry = state.entry(grant.to_string()).or_insert((now, 0));
        if entry.1 >= self.limit {
            return Err(self.window.saturating_sub(now.duration_since(entry.0)));
        }
        entry.1 += 1;
        Ok(())
    }
}
