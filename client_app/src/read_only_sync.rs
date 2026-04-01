#[cfg_attr(not(test), allow(dead_code))]
pub const READ_ONLY_POLL_INTERVAL_MS: u32 = 60_000;
#[cfg_attr(not(test), allow(dead_code))]
pub const READ_ONLY_MAX_POLL_INTERVAL_MS: u32 = 600_000;
pub const READ_ONLY_ACTIVATION_REFRESH_DEBOUNCE_MS: f64 = 60_000.0;

/// Quiet read-only views can back off after repeated empty polls without
/// impacting score-entry latency, because the user can still force a refresh.
#[cfg_attr(not(test), allow(dead_code))]
pub fn next_read_only_poll_interval_ms(current_ms: u32, saw_new_events: bool) -> u32 {
    if saw_new_events {
        return READ_ONLY_POLL_INTERVAL_MS;
    }

    current_ms
        .max(READ_ONLY_POLL_INTERVAL_MS)
        .saturating_mul(2)
        .min(READ_ONLY_MAX_POLL_INTERVAL_MS)
}

/// Read-only tabs should refresh when the user clearly returns, but not so
/// often that brief app switches create bursty worker traffic.
pub fn should_run_debounced_activation_refresh(
    now_ms: f64,
    last_activation_refresh_ms: Option<f64>,
) -> bool {
    match last_activation_refresh_ms {
        Some(last_ms) => now_ms - last_ms >= READ_ONLY_ACTIVATION_REFRESH_DEBOUNCE_MS,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        next_read_only_poll_interval_ms, should_run_debounced_activation_refresh,
        READ_ONLY_ACTIVATION_REFRESH_DEBOUNCE_MS, READ_ONLY_MAX_POLL_INTERVAL_MS,
        READ_ONLY_POLL_INTERVAL_MS,
    };

    #[test]
    fn quiet_sessions_back_off_until_the_cap() {
        assert_eq!(
            next_read_only_poll_interval_ms(READ_ONLY_POLL_INTERVAL_MS, false),
            120_000
        );
        assert_eq!(
            next_read_only_poll_interval_ms(300_000, false),
            READ_ONLY_MAX_POLL_INTERVAL_MS
        );
        assert_eq!(
            next_read_only_poll_interval_ms(READ_ONLY_MAX_POLL_INTERVAL_MS, false),
            READ_ONLY_MAX_POLL_INTERVAL_MS
        );
    }

    #[test]
    fn new_events_reset_polling_to_the_fast_interval() {
        assert_eq!(
            next_read_only_poll_interval_ms(READ_ONLY_MAX_POLL_INTERVAL_MS, true),
            READ_ONLY_POLL_INTERVAL_MS
        );
    }

    #[test]
    fn activation_refresh_runs_immediately_when_never_run_before() {
        assert!(should_run_debounced_activation_refresh(1_000.0, None));
    }

    #[test]
    fn activation_refresh_is_suppressed_inside_the_debounce_window() {
        assert!(!should_run_debounced_activation_refresh(
            90_000.0,
            Some(90_000.0 - (READ_ONLY_ACTIVATION_REFRESH_DEBOUNCE_MS - 1.0)),
        ));
    }

    #[test]
    fn activation_refresh_runs_again_after_the_debounce_window() {
        assert!(should_run_debounced_activation_refresh(
            90_000.0,
            Some(90_000.0 - READ_ONLY_ACTIVATION_REFRESH_DEBOUNCE_MS),
        ));
    }
}
