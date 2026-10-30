//! Is an incoming stream healthy?  Used to warn (and log) when the partner's
//! packets arrive but nothing can be decoded, e.g. an incompatible decoder.

use std::time::Duration;

/// How long a stream may go without decoded output before we complain.
pub const GRACE: Duration = Duration::from_secs(5);

/// True if packets for a stream keep arriving but none of them turned into
/// a picture or sound for longer than [`GRACE`].  A stream that simply does
/// not arrive (the partner has no camera, or nothing is shared) is not
/// reported here.
pub fn arrives_but_not_decoded(
    since_start: Duration,
    since_packet: Option<Duration>,
    since_decoded: Option<Duration>,
) -> bool {
    let receiving = since_packet.is_some_and(|d| d < Duration::from_secs(2));
    let decoding = since_decoded.is_some_and(|d| d < GRACE);
    since_start >= GRACE && receiving && !decoding
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn healthy_and_silent_streams_are_fine() {
        assert!(
            !arrives_but_not_decoded(S(60), Some(S(0)), Some(S(0))),
            "healthy"
        );
        assert!(
            !arrives_but_not_decoded(S(60), None, None),
            "nothing sent (no camera)"
        );
        assert!(
            !arrives_but_not_decoded(S(60), Some(S(30)), Some(S(30))),
            "stopped sending"
        );
    }

    #[test]
    fn packets_without_output_are_reported_after_the_grace_time() {
        assert!(
            !arrives_but_not_decoded(S(3), Some(S(0)), None),
            "too early"
        );
        assert!(
            arrives_but_not_decoded(S(6), Some(S(0)), None),
            "never decoded"
        );
        assert!(
            arrives_but_not_decoded(S(60), Some(S(0)), Some(S(10))),
            "decoding stopped"
        );
    }
}
