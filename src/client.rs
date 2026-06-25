//! A minimal *typed* CDP client over the CBOR pipe.
//!
//! Commands are the strongly-typed structs from `chromiumoxide_cdp` (which
//! implement `chromiumoxide_types::Command`). We wrap each one in the CDP
//! request envelope `{ id, method, params }`, encode it with our crdtp-CBOR
//! serializer, and decode the reply into the command's associated
//! `Response` type.

use crate::cbor;
use crate::pipe::PipeConn;
use chromiumoxide_types::Command;
use serde::Serialize;

pub struct Client {
    conn: PipeConn,
    next_id: u64,
    /// Reused across `execute` calls so the hot send path doesn't allocate a
    /// fresh encode buffer per command (see `cbor::to_buf`).
    enc_buf: Vec<u8>,
}

/// The on-the-wire CDP request. `params` serializes inline as the command's
/// own map, exactly like the JSON transport but in crdtp CBOR.
#[derive(Serialize)]
struct Request<'a, C: Serialize> {
    id: u64,
    method: &'a str,
    params: &'a C,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Encode(String),
    Decode(String),
    /// A CDP protocol error `{code, message}` returned by the browser.
    Protocol { code: i64, message: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Encode(e) => write!(f, "encode: {e}"),
            Error::Decode(e) => write!(f, "decode: {e}"),
            Error::Protocol { code, message } => write!(f, "cdp error {code}: {message}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl Client {
    pub fn new(conn: PipeConn) -> Self {
        Client { conn, next_id: 1, enc_buf: Vec::with_capacity(4096) }
    }

    /// Send a typed command (optionally on a session) and return its typed
    /// response, pumping the message stream until the matching `id` arrives.
    pub fn execute<C: Command>(
        &mut self,
        cmd: &C,
        session_id: Option<&str>,
    ) -> Result<C::Response, Error> {
        let id = self.next_id;
        self.next_id += 1;

        let method = cmd.identifier();
        let req = Request {
            id,
            method: method.as_ref(),
            params: cmd,
            session_id,
        };
        self.enc_buf.clear();
        cbor::to_buf(&req, &mut self.enc_buf).map_err(|e| Error::Encode(e.0))?;
        // Take the buffer out so we can borrow `self.conn` mutably to send,
        // then return it for reuse on the next call.
        let buf = std::mem::take(&mut self.enc_buf);
        let send = self.conn.send_raw(&buf);
        self.enc_buf = buf;
        send?;

        // Pump messages until we see the response with our id. We decode each
        // frame *directly* from CBOR into the typed reply in a single pass —
        // no intermediate `serde_json::Value` tree. This is sound for this
        // synchronous client because the only `result`-bearing message in
        // flight is the reply to the one command we just sent; every other
        // inbound message is an event (a `method`/`params` pair with no `id`
        // and no `result`), whose extra fields the typed `Reply` simply
        // ignores, leaving `id = None` so it is skipped.
        loop {
            let frame = self.conn.recv_raw()?;
            let reply: Reply<C::Response> =
                cbor::from_slice(&frame).map_err(|e| Error::Decode(e.0))?;

            if reply.id != Some(id) {
                continue; // event or unrelated message
            }
            if let Some(err) = reply.error {
                return Err(Error::Protocol {
                    code: err.code,
                    message: err.message,
                });
            }
            return reply
                .result
                .ok_or_else(|| Error::Decode("response missing result".into()));
        }
    }
}

/// A typed CDP reply envelope, deserialized directly from CBOR. Unknown fields
/// (e.g. an event's `method`/`params`) are ignored by serde, so non-matching
/// messages decode harmlessly with `id = None`.
#[derive(serde::Deserialize)]
struct Reply<R> {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default = "none")]
    result: Option<R>,
    #[serde(default)]
    error: Option<ProtocolError>,
}

#[derive(serde::Deserialize)]
struct ProtocolError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

fn none<R>() -> Option<R> {
    None
}
