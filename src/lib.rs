//! rust-cdp: a minimal typed Chrome DevTools Protocol client that speaks
//! Chrome's CBOR dialect ("crdtp") over `--remote-debugging-pipe=cbor`.
//!
//! The library exposes the pieces so benchmarks/examples can reuse them:
//!   * [`cbor`]   — serde codec for the crdtp CBOR dialect
//!   * [`client`] — typed request/response client over the pipe
//!   * [`pipe`]   — spawn Chrome and frame messages over fd 3/4

pub mod cbor;
pub mod client;
pub mod pipe;
