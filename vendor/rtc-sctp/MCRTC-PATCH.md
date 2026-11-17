# Patched rtc-sctp 0.21.1

A copy of `rtc-sctp` 0.21.1 from crates.io, used through `[patch.crates-io]` in the root
`Cargo.toml`, because `rtc` builds its SCTP transport with default timers and offers no way to
change them. Only `src/association/timer.rs` differs, in four places marked `mcrtc:`, plus one
test in `src/endpoint/endpoint_test.rs` that hardcoded the old delayed-ack interval:

- `max_t3_rtx_retrans` is unlimited. Upstream gives up on DATA retransmissions after 5 timeouts
  without closing the association, so after an outage of about 30 seconds lost messages are never
  resent and ordered channels, including `control`, stall for good.
- `RTO_MAX` is 5 seconds instead of 60, so a link that comes back is used again within seconds.
- `RTO_MIN` is 200 ms instead of 1 second. A single lost packet otherwise froze its ordered
  channel, and everything queued behind it, for at least a second. Above the floor the timeout
  still follows the measured RTT (srtt + 4 * rttvar), so a slow link does not resend early.
- `ACK_INTERVAL` (delayed SACK) is 50 ms instead of 200, so the sender learns of losses and can
  grow its window sooner with Minecraft's sparse traffic.

Three upstream tests fail with these limits because they assume the old `RTO_MAX`:
`public_rtt_reports_measurements_independently_of_backoff`,
`t2_backoff_does_not_compound_simultaneous_t3_expirations` and
`control_backoff_uses_one_interval_until_stopped`.

When upgrading `rtc`, copy the matching `rtc-sctp` release here and reapply these changes, or drop
the patch if upstream exposes these settings.
