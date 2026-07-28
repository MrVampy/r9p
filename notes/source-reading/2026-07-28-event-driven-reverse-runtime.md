# Event-driven reverse runtime

Date: 2026-07-28.

## Question

How should reverse exporters, brokers, and local session proxies wait for
connection state without periodic sleeps?

## Sources inspected

- `crates/reverse/src/export.rs`
- `crates/reverse/src/broker.rs`
- `crates/reverse/src/session_proxy.rs`
- `crates/reverse/src/tests.rs`
- Agents `crates/runner/src/profile_service/runtime.rs`
- Agents `crates/runner/src/operating_service/runtime.rs`

## Findings

The reverse exporter published only an atomic connected-stream count. Callers
therefore sampled it every 25 ms during startup. Its reconnect backoff also
used 25 ms sleep quanta solely so shutdown would eventually be noticed.

Both broker listeners and the local session-proxy listener were nonblocking and
sampled `accept` every 25 ms. Their drop implementations already opened an
explicit wake connection to each listener, so nonblocking accept added idle
wakeups without adding a cancellation capability.

The generic mechanism has exact events for every transition:

- a reverse worker adds or removes a connected stream;
- a broker handshake adds or removes a waiting stream;
- a retry deadline expires;
- shutdown changes lifecycle state; and
- a listener receives either a real connection or its explicit shutdown wake.

## Effect

Reverse export and broker availability now use condition-variable
notifications with bounded wait methods. Retry backoff waits on the same
shutdown-aware condition instead of sleeping in quanta. Broker and
session-proxy listeners use blocking accept and retain their explicit wake
connections for shutdown.

Consumers can inspect instantaneous counters or block for a declared
availability deadline without constructing their own polling loop.
