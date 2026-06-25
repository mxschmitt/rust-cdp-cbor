//! A serde codec for Chrome DevTools' CBOR dialect ("crdtp" CBOR).
//!
//! Chrome's `--remote-debugging-pipe=cbor` mode exchanges messages encoded
//! with the inspector-protocol CBOR variant, which differs from RFC 7049 CBOR
//! in a few deliberate ways (enveloped + indefinite-length maps/arrays,
//! int32-range scalars, UTF-8/UTF-16 strings). Off-the-shelf CBOR crates
//! cannot read or write it, so this module implements the dialect directly on
//! top of serde — letting any serde-derived CDP type round-trip unchanged.

pub mod consts;
pub mod de;
pub mod ser;

pub use de::{from_slice, message_len};
pub use ser::to_vec;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Navigate {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        referrer: Option<String>,
        transition_count: i32,
        active: bool,
        ratio: f64,
        frames: Vec<String>,
    }

    #[test]
    fn round_trip() {
        let n = Navigate {
            url: "https://example.com".into(),
            referrer: None,
            transition_count: -7,
            active: true,
            ratio: 1.5,
            frames: vec!["a".into(), "b".into()],
        };
        let bytes = to_vec(&n).unwrap();
        // Top-level message must look like a crdtp envelope.
        assert!(consts::is_cbor_message(&bytes), "bytes: {bytes:02x?}");
        // The declared frame length must cover exactly the buffer.
        assert_eq!(message_len(&bytes).unwrap(), Some(bytes.len()));
        let back: Navigate = from_slice(&bytes).unwrap();
        assert_eq!(n, back);
    }

    #[test]
    fn envelope_bytes() {
        // A minimal {} map should be: D8 18 5A 00000001 BF FF
        #[derive(Serialize)]
        struct Empty {}
        let bytes = to_vec(&Empty {}).unwrap();
        assert_eq!(&bytes[..3], &[0xD8, 0x18, 0x5A]);
        let len = u32::from_be_bytes(bytes[3..7].try_into().unwrap());
        assert_eq!(len, 2); // BF FF
        assert_eq!(&bytes[7..], &[0xBF, 0xFF]);
    }
}
