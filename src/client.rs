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
        Client { conn, next_id: 1 }
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
        let bytes = cbor::to_vec(&req).map_err(|e| Error::Encode(e.0))?;
        self.conn.send_raw(&bytes)?;

        // Pump messages until we see the response with our id. Events and
        // responses to other ids are skipped.
        loop {
            let frame = self.conn.recv_raw()?;
            let msg: serde_json::Value =
                cbor::from_slice(&frame).map_err(|e| Error::Decode(e.0))?;

            // Events have a "method" but no "id"; skip them.
            let msg_id = msg.get("id").and_then(|v| v.as_u64());
            if msg_id != Some(id) {
                continue;
            }

            if let Some(err) = msg.get("error") {
                let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
                let message = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                return Err(Error::Protocol { code, message });
            }

            let result = msg
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Object(Default::default()));
            return serde_json::from_value::<C::Response>(result)
                .map_err(|e| Error::Decode(e.to_string()));
        }
    }
}
