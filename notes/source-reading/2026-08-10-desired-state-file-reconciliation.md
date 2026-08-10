# Desired-state file reconciliation

## Question

How should a service publisher recover when a namespace path is still visible
through a retained Front image but the replacement backend no longer has the
application record addressed by a write to that path?

## Sources inspected

- `crates/session/src/client/paths.rs`
  - `write_file` walks and replaces one existing file.
  - `create_write_at` walks the parent, creates only the final path element,
    and writes the initial contents.
- `refs/plan9port/src/cmd/9p.c`
  - `xwrite` opens an existing file with `OWRITE|OTRUNC` and writes it.
  - `xcreate` uses the separate `fscreate` operation.
- `refs/plan9port/include/9pclient.h`
  - The established client API keeps `fsopen`, `fswrite`, and `fscreate` as
    distinct protocol operations.

## Decision

Keep the protocol operations distinct and add desired-state reconciliation
only to the higher-level session client. `Client::reconcile_file_at` first
replaces the addressed file. A definitive `ENOENT` selects create; a
definitive concurrent-create `EEXIST` selects one final replacement. No other
write failure is retried, because delivery may be ambiguous.

This remains backend-neutral and useful to any 9P publisher. Service lease,
registration, and admission meaning stays outside r9p.
