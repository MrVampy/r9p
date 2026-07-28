# Pipelined RPC Delivery Classification

Date: 2026-07-28

## Question

Can a client that pipelines a 9P `Twrite` and dependent `Tread` distinguish a
definitive server rejection of the write from loss of the response after the
write may have committed?

## Sources inspected

- `crates/core/src/client.rs`
- `crates/core/src/multiplex/client.rs`
- `crates/core/src/multiplex/reader.rs`
- `crates/session/src/client/direct.rs`
- `crates/session/src/client/namespace.rs`
- `crates/session/src/opened_fid.rs`
- `crates/session/src/error.rs`

## Findings

- 9P already provides the decisive boundary. An `Rerror` carrying the exact
  `Twrite` tag is a definitive rejection of that write.
- A transport failure or timeout before the write reply leaves delivery
  uncertain because some or all of the frame may have reached the server.
- A valid `Rwrite` followed by a failed, malformed, or lost dependent read
  means the request was accepted but its application response is uncertain.
- The previous combined helper reduced all three cases to one unstructured
  error. An application therefore could not decide whether to report a
  rejection or perform idempotent result reconciliation.
- The client must still consume both pipelined replies before classifying the
  pair. Returning as soon as one waiter fails would abandon live protocol
  state for the other tag.

## Effect

The multiplexed client and session facade now preserve two outcomes:
`Rejected` and `DeliveryUnknown`. This remains generic 9P client mechanism.
Application clients decide how to reconcile an unknown operation through their
own namespace contract.

## Open questions

Large multi-frame writes remain conservatively classified as delivery unknown
after any prefix write has been attempted. No current terminal RPC reaches
that path because its request bound is below the negotiated write payload.
