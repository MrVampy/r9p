# Front handler reuse through reverse export

Date: 2026-07-30

## Question

How can an application publish a mutable `r9p-front` tree through
`ReverseExport` while preserving blocking-read cancellation and `Tflush`
behavior?

## Sources inspected

- `crates/front/src/serve.rs`
  - `serve_front_connection`
  - `FrontConnectionHandler::perform`
  - `FrontConnectionHandler::is_async`
  - `FrontConnectionHandler::cancellation_fid`
  - `FrontConnectionHandler::wake_after_cancel`
- `crates/front/src/front.rs`
  - `Front::register_log`
  - `Front::append_event`
  - `Front::wake_readers`
- `crates/reverse/src/export.rs`
  - `ReverseExport::start_authenticated_handler`
- `crates/core/src/server/serve.rs`
  - `ConnectionHandler`
  - asynchronous request cancellation

## Findings

`FrontTree` provides the ordinary `FileTree` surface, but the Front TCP server
uses a dedicated `ConnectionHandler` to make reads asynchronous, associate
cancellation with the read fid, and wake blocked Front readers after
cancellation. `ReverseExport` already accepts an authenticated
`ConnectionHandler` factory, so no reverse-transport or protocol change is
needed.

The missing seam was only construction visibility. `Front` now exposes its
existing cancellation-aware handler through `Front::connection_handler`.
Applications can pass that handler to
`ReverseExport::start_authenticated_handler` and retain the same behavior as
`Front::serve_tcp_authenticated`.

## Effect

Applications should reuse the public Front handler for reverse-published
mutable trees with blocking reads. They should not copy the dispatcher or use
the simpler `FileTree` reverse path when cancellation is part of the contract.

## Open questions

None.
