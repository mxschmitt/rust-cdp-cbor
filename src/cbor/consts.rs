//! Byte constants for Chrome's crdtp CBOR dialect.
//!
//! Values mirror `third_party/inspector_protocol/crdtp/cbor.cc`:
//! https://source.chromium.org/chromium/chromium/src/+/main:third_party/inspector_protocol/crdtp/cbor.cc
//! CBOR's
//! initial byte packs a 3-bit major type (high bits) and 5-bit additional
//! information (low bits): `initial = (major << 5) | info`.

pub const MAJOR_TYPE_SHIFT: u8 = 5;

// Major types (RFC 7049 §2.1).
pub const MAJOR_UNSIGNED: u8 = 0;
pub const MAJOR_NEGATIVE: u8 = 1;
pub const MAJOR_BYTE_STRING: u8 = 2;
pub const MAJOR_STRING: u8 = 3;
pub const MAJOR_ARRAY: u8 = 4;
pub const MAJOR_MAP: u8 = 5;
pub const MAJOR_TAG: u8 = 6;
pub const MAJOR_SIMPLE: u8 = 7;

// Additional-information codes that mean "payload follows in N bytes".
pub const INFO_1BYTE: u8 = 24;
pub const INFO_2BYTES: u8 = 25;
pub const INFO_4BYTES: u8 = 26;
pub const INFO_8BYTES: u8 = 27;
pub const INFO_MASK: u8 = 0x1f;
pub const MAJOR_MASK: u8 = 0xe0;

// Envelope: TAG(major 6) + info 24 -> 0xD8, then the standalone tag value 24
// (0x18), then a 32-bit-length BYTE_STRING initial byte.
pub const INITIAL_BYTE_ENVELOPE: u8 = (MAJOR_TAG << MAJOR_TYPE_SHIFT) | INFO_1BYTE; // 0xD8
pub const CBOR_ENVELOPE_TAG: u8 = 24; // 0x18
pub const INITIAL_BYTE_32BIT_BYTESTRING: u8 = (MAJOR_BYTE_STRING << MAJOR_TYPE_SHIFT) | INFO_4BYTES; // 0x5A

// Indefinite-length compounds and the stop byte.
pub const INDEF_ARRAY_START: u8 = (MAJOR_ARRAY << MAJOR_TYPE_SHIFT) | 31; // 0x9F
pub const INDEF_MAP_START: u8 = (MAJOR_MAP << MAJOR_TYPE_SHIFT) | 31; // 0xBF
pub const STOP_BYTE: u8 = (MAJOR_SIMPLE << MAJOR_TYPE_SHIFT) | 31; // 0xFF

// Simple values.
pub const ENCODED_FALSE: u8 = (MAJOR_SIMPLE << MAJOR_TYPE_SHIFT) | 20; // 0xF4
pub const ENCODED_TRUE: u8 = (MAJOR_SIMPLE << MAJOR_TYPE_SHIFT) | 21; // 0xF5
pub const ENCODED_NULL: u8 = (MAJOR_SIMPLE << MAJOR_TYPE_SHIFT) | 22; // 0xF6
pub const INITIAL_BYTE_DOUBLE: u8 = (MAJOR_SIMPLE << MAJOR_TYPE_SHIFT) | INFO_8BYTES; // 0xFB

// Tag 22: "expect base64 conversion" prefix for binary byte strings.
pub const EXPECTED_CONVERSION_TO_BASE64_TAG: u8 = (MAJOR_TAG << MAJOR_TYPE_SHIFT) | 22; // 0xD6

/// True iff `msg` begins like a crdtp CBOR envelope (0xD8 then either the
/// 0x18 tag byte + 0x5A, or a bare 0x5A for the legacy one-byte form).
#[allow(dead_code)] // used by tests and as a public sanity-check helper
pub fn is_cbor_message(msg: &[u8]) -> bool {
    msg.len() >= 4
        && msg[0] == INITIAL_BYTE_ENVELOPE
        && (msg[1] == INITIAL_BYTE_32BIT_BYTESTRING
            || (msg[1] == CBOR_ENVELOPE_TAG && msg[2] == INITIAL_BYTE_32BIT_BYTESTRING))
}
