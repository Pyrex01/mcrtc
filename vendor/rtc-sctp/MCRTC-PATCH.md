# Patched rtc-sctp 0.21.1

A copy of `rtc-sctp` 0.21.1 from crates.io, used through `[patch.crates-io]` in the root
`Cargo.toml`, because `rtc` builds its SCTP transport with default timers and offers no way to
change them. Only `src/association/timer.rs` differs, in two places marked `mcrtc:`:

- `max_t3_rtx_retrans` is unlimited. Upstream gives up on DATA retransmissions after 5 timeouts
  without closing the association, so after an outage of about 30 seconds lost messages are never
  resent and ordered channels, including `control`, stall for good.
- `RTO_MAX` is 5 seconds instead of 60, so a link that comes back is used again within seconds.

When upgrading `rtc`, copy the matching `rtc-sctp` release here and reapply both changes, or drop
the patch if upstream exposes these settings.
