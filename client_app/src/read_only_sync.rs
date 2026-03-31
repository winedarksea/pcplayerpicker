pub const READ_ONLY_POLL_INTERVAL_MS: u32 = 60_000;
pub const READ_ONLY_MAX_POLL_INTERVAL_MS: u32 = 300_000;

/// Quiet read-only views can back off after repeated empty polls without
/// impacting score-entry latency, because the user can still force a refresh.
pub fn next_read_only_poll_interval_ms(current_ms: u32, saw_new_events: bool) -> u32 {
    if saw_new_events {
        return READ_ONLY_POLL_INTERVAL_MS;
    }

    current_ms
        .max(READ_ONLY_POLL_INTERVAL_MS)
        .saturating_mul(2)
        .min(READ_ONLY_MAX_POLL_INTERVAL_MS)
}

#[cfg(test)]
mod tests {
    use super::{
        next_read_only_poll_interval_ms, READ_ONLY_MAX_POLL_INTERVAL_MS, READ_ONLY_POLL_INTERVAL_MS,
    };

    #[test]
    fn quiet_sessions_back_off_until_the_cap() {
        assert_eq!(
            next_read_only_poll_interval_ms(READ_ONLY_POLL_INTERVAL_MS, false),
            120_000
        );
        assert_eq!(
            next_read_only_poll_interval_ms(240_000, false),
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
}
