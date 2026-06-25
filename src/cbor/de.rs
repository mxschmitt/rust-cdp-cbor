//! Serde `Deserializer` for Chrome's crdtp CBOR dialect.
//!
//! Mirrors the encoder in `ser.rs`: it transparently steps over envelopes
//! (tag 24 + 4-byte byte-string header) wherever a map or array is expected,
//! and decodes indefinite-length maps/arrays terminated by the stop byte.
//!
//! Wire format reference (`third_party/inspector_protocol/crdtp/cbor.cc`):
//! https://source.chromium.org/chromium/chromium/src/+/main:third_party/inspector_protocol/crdtp/cbor.cc

use super::consts::*;
use serde::de::{self, Deserialize, DeserializeOwned, IntoDeserializer};
use std::fmt::Display;

#[derive(Debug)]
pub struct Error(pub String);

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cbor de: {}", self.0)
    }
}
impl std::error::Error for Error {}
impl de::Error for Error {
    fn custom<T: Display>(msg: T) -> Self {
        Error(msg.to_string())
    }
}

type Result<T> = std::result::Result<T, Error>;

pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let mut de = Deserializer { input: bytes, pos: 0 };
    let value = T::deserialize(&mut de)?;
    Ok(value)
}

pub struct Deserializer<'de> {
    input: &'de [u8],
    pos: usize,
}

impl<'de> Deserializer<'de> {
    fn peek(&self) -> Result<u8> {
        self.input.get(self.pos).copied().ok_or_else(|| Error("unexpected EOF".into()))
    }
    fn take(&mut self) -> Result<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Ok(b)
    }
    fn take_n(&mut self, n: usize) -> Result<&'de [u8]> {
        if self.pos + n > self.input.len() {
            return Err(Error("unexpected EOF".into()));
        }
        let s = &self.input[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// Read a token start, returning (major_type, value). For BYTE_STRING /
    /// STRING the value is the length; for ints it is the payload.
    fn read_token(&mut self) -> Result<(u8, u64)> {
        let initial = self.take()?;
        let major = (initial & MAJOR_MASK) >> MAJOR_TYPE_SHIFT;
        let info = initial & INFO_MASK;
        let value = match info {
            0..=23 => info as u64,
            INFO_1BYTE => self.take()? as u64,
            INFO_2BYTES => u16::from_be_bytes(self.take_n(2)?.try_into().unwrap()) as u64,
            INFO_4BYTES => u32::from_be_bytes(self.take_n(4)?.try_into().unwrap()) as u64,
            INFO_8BYTES => u64::from_be_bytes(self.take_n(8)?.try_into().unwrap()),
            31 => u64::MAX, // indefinite marker; caller interprets
            _ => return Err(Error(format!("bad additional info {info}"))),
        };
        Ok((major, value))
    }

    /// Step past an envelope header (tag + byte-string length) so the cursor
    /// lands on the wrapped map/array initial byte, and return the absolute
    /// offset of the envelope's end (`content start + content length`).
    ///
    /// Snapping to this end after decoding the contents makes us robust to how
    /// much the serde visitor actually consumed — e.g. a fixed-size tuple
    /// `[i32; N]` reads exactly N elements and never consumes the indefinite
    /// array's trailing stop byte, but the envelope length still bounds it.
    fn enter_envelope(&mut self) -> Result<usize> {
        self.pos += 1; // 0xD8
        if self.peek()? == CBOR_ENVELOPE_TAG {
            self.pos += 1; // 0x18
        }
        let (major, len) = self.read_token()?;
        if major != MAJOR_BYTE_STRING {
            return Err(Error("envelope missing byte-string header".into()));
        }
        Ok(self.pos + len as usize)
    }

    /// If the cursor sits on an envelope, step past its header (ignoring the
    /// returned end offset). Used where the contents are read by a stop-byte
    /// terminated visitor that doesn't need the bound.
    fn skip_envelope(&mut self) -> Result<()> {
        if self.peek()? == INITIAL_BYTE_ENVELOPE {
            self.enter_envelope()?;
        }
        Ok(())
    }

    fn read_str(&mut self) -> Result<&'de str> {
        let (major, len) = self.read_token()?;
        if major != MAJOR_STRING {
            return Err(Error(format!("expected string, major {major}")));
        }
        let bytes = self.take_n(len as usize)?;
        std::str::from_utf8(bytes).map_err(|e| Error(e.to_string()))
    }
}

impl<'de, 'a> de::Deserializer<'de> for &'a mut Deserializer<'de> {
    type Error = Error;

    fn deserialize_any<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let b = self.peek()?;
        // Envelope -> the wrapped value (always a map or array in CDP). Decode
        // the contents, then snap the cursor to the envelope's declared end so
        // any unconsumed trailing bytes (e.g. a stop byte after a fixed-size
        // tuple) are skipped.
        if b == INITIAL_BYTE_ENVELOPE {
            let end = self.enter_envelope()?;
            let value = self.deserialize_any(visitor)?;
            self.pos = end;
            return Ok(value);
        }
        let major = (b & MAJOR_MASK) >> MAJOR_TYPE_SHIFT;
        let info = b & INFO_MASK;
        match (major, info) {
            (MAJOR_UNSIGNED, _) => {
                let (_, v) = self.read_token()?;
                visitor.visit_i64(v as i64)
            }
            (MAJOR_NEGATIVE, _) => {
                let (_, v) = self.read_token()?;
                visitor.visit_i64(-1 - v as i64)
            }
            (MAJOR_BYTE_STRING, _) => {
                let (_, len) = self.read_token()?;
                let bytes = self.take_n(len as usize)?;
                visitor.visit_borrowed_bytes(bytes)
            }
            (MAJOR_STRING, _) => visitor.visit_borrowed_str(self.read_str()?),
            (MAJOR_ARRAY, 31) => {
                self.pos += 1;
                visitor.visit_seq(Indef { de: self })
            }
            (MAJOR_MAP, 31) => {
                self.pos += 1;
                visitor.visit_map(Indef { de: self })
            }
            (MAJOR_SIMPLE, 20) => {
                self.pos += 1;
                visitor.visit_bool(false)
            }
            (MAJOR_SIMPLE, 21) => {
                self.pos += 1;
                visitor.visit_bool(true)
            }
            (MAJOR_SIMPLE, 22) => {
                self.pos += 1;
                visitor.visit_unit()
            }
            (MAJOR_SIMPLE, INFO_8BYTES) => {
                self.pos += 1;
                let bytes = self.take_n(8)?;
                visitor.visit_f64(f64::from_be_bytes(bytes.try_into().unwrap()))
            }
            (MAJOR_TAG, _) => {
                // Skip any non-envelope tag (e.g. base64-binary tag 22).
                let _ = self.read_token()?;
                self.deserialize_any(visitor)
            }
            _ => Err(Error(format!("unsupported initial byte 0x{b:02x}"))),
        }
    }

    fn deserialize_option<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        if self.peek()? == ENCODED_NULL {
            self.pos += 1;
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_unit<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        if self.peek()? == ENCODED_NULL {
            self.pos += 1;
        }
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        // CDP enums are plain strings (unit variants); externally-tagged
        // object enums also work via the map form.
        let b = self.peek()?;
        if b == INITIAL_BYTE_ENVELOPE
            || ((b & MAJOR_MASK) >> MAJOR_TYPE_SHIFT) == MAJOR_MAP
        {
            self.skip_envelope()?;
            self.pos += 1; // map start
            let value = visitor.visit_enum(EnumAccess { de: self })?;
            // consume trailing stop byte of the map
            if self.peek()? == STOP_BYTE {
                self.pos += 1;
            }
            Ok(value)
        } else {
            let s = self.read_str()?;
            visitor.visit_enum(s.into_deserializer())
        }
    }

    // Everything else routes through deserialize_any.
    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string
        bytes byte_buf seq tuple tuple_struct map struct identifier
        unit_struct ignored_any
    }
}

/// Sequence/map access for indefinite-length compounds (until stop byte).
struct Indef<'a, 'de: 'a> {
    de: &'a mut Deserializer<'de>,
}

impl<'a, 'de> de::SeqAccess<'de> for Indef<'a, 'de> {
    type Error = Error;
    fn next_element_seed<T: de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>> {
        if self.de.peek()? == STOP_BYTE {
            self.de.pos += 1;
            return Ok(None);
        }
        seed.deserialize(&mut *self.de).map(Some)
    }
}

impl<'a, 'de> de::MapAccess<'de> for Indef<'a, 'de> {
    type Error = Error;
    fn next_key_seed<K: de::DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        if self.de.peek()? == STOP_BYTE {
            self.de.pos += 1;
            return Ok(None);
        }
        seed.deserialize(&mut *self.de).map(Some)
    }
    fn next_value_seed<V: de::DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        seed.deserialize(&mut *self.de)
    }
}

/// Enum access for externally-tagged object enums: `{ "Variant": value }`.
struct EnumAccess<'a, 'de: 'a> {
    de: &'a mut Deserializer<'de>,
}

impl<'a, 'de> de::EnumAccess<'de> for EnumAccess<'a, 'de> {
    type Error = Error;
    type Variant = Self;
    fn variant_seed<V: de::DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant)> {
        let key = seed.deserialize(&mut *self.de)?;
        Ok((key, self))
    }
}

impl<'a, 'de> de::VariantAccess<'de> for EnumAccess<'a, 'de> {
    type Error = Error;
    fn unit_variant(self) -> Result<()> {
        Ok(())
    }
    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        seed.deserialize(&mut *self.de)
    }
    fn tuple_variant<V: de::Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        de::Deserializer::deserialize_any(&mut *self.de, visitor)
    }
    fn struct_variant<V: de::Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        de::Deserializer::deserialize_any(&mut *self.de, visitor)
    }
}

/// Parse just the top-level envelope payload bounds: returns the byte length of
/// the whole message starting at `bytes[0]`, used by the pipe reader to know
/// how many bytes form one frame. Returns `None` if more bytes are needed.
pub fn message_len(bytes: &[u8]) -> Result<Option<usize>> {
    // Envelope header: 0xD8 [0x18] 0x5A <u32 be len>
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes[0] != INITIAL_BYTE_ENVELOPE {
        return Err(Error(format!("not an envelope: 0x{:02x}", bytes[0])));
    }
    let mut off = 1;
    if bytes.get(off) == Some(&CBOR_ENVELOPE_TAG) {
        off += 1;
    }
    if bytes.get(off) != Some(&INITIAL_BYTE_32BIT_BYTESTRING) {
        // header not fully arrived yet (or legacy form we don't emit)
        return Ok(None);
    }
    off += 1;
    if bytes.len() < off + 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
    Ok(Some(off + 4 + len))
}

/// Convenience used by tests/round-trips.
#[allow(dead_code)]
pub fn decode<'de, T: Deserialize<'de>>(bytes: &'de [u8]) -> Result<T> {
    let mut de = Deserializer { input: bytes, pos: 0 };
    T::deserialize(&mut de)
}
