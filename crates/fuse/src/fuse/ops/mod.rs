//! Per-opcode FUSE request handlers.
//!
//! Each submodule contributes one or more `impl R9pFuse` blocks containing
//! the methods dispatched from `super::dispatch`.

mod attrs;
mod create;
mod dir;
mod io;
mod locks;
mod lookup;
mod misc;
mod mutate;

#[cfg(test)]
pub(super) use dir::encode_dirents;
