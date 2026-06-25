//! Serde `Serializer` that emits Chrome DevTools' CBOR dialect (crdtp).
//!
//! This is *not* standard CBOR. The rules (from
//! `third_party/inspector_protocol/crdtp/cbor.{h,cc}`,
//! https://source.chromium.org/chromium/chromium/src/+/main:third_party/inspector_protocol/crdtp/cbor.h)
//! are:
//!   * Every map and array is wrapped in an "envelope": tag(24) + a 4-byte
//!     byte-string header whose payload length is the wrapped item's size.
//!   * Maps and arrays use *indefinite* length (0xBF / 0x9F ... 0xFF).
//!   * Integers stay in the int32 range, encoded as major type 0/1.
//!   * Text is emitted as UTF-8 STRING (major type 3).
//!
//! See `super::consts` for the exact byte constants.

use super::consts::*;
use serde::{ser, Serialize};
use std::fmt::Display;

#[derive(Debug)]
pub struct Error(pub String);

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cbor ser: {}", self.0)
    }
}
impl std::error::Error for Error {}
impl ser::Error for Error {
    fn custom<T: Display>(msg: T) -> Self {
        Error(msg.to_string())
    }
}

type Result<T> = std::result::Result<T, Error>;

/// Serialize `value` into a complete crdtp CBOR message (a top-level
/// enveloped, indefinite-length map).
pub fn to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut s = Serializer { out: Vec::new() };
    value.serialize(&mut s)?;
    Ok(s.out)
}

/// Serialize `value` by appending to a caller-owned buffer. Lets a hot send
/// loop reuse one allocation across many messages (`buf.clear()` between
/// calls) instead of allocating a fresh `Vec` each time.
pub fn to_buf<T: Serialize>(value: &T, buf: &mut Vec<u8>) -> Result<()> {
    // Move the buffer into the Serializer and back to avoid copying; the
    // Serializer owns a Vec, so we swap to reuse the caller's allocation.
    let mut s = Serializer { out: std::mem::take(buf) };
    let r = value.serialize(&mut s);
    *buf = s.out;
    r
}

pub struct Serializer {
    out: Vec<u8>,
}

impl Serializer {
    /// Write a token start: initial byte for `major`, packing `value` either
    /// inline or as a 1/2/4/8-byte big-endian payload (RFC 7049 §2.1).
    #[inline]
    fn write_token_start(&mut self, major: u8, value: u64) {
        let mt = major << MAJOR_TYPE_SHIFT;
        if value < 24 {
            self.out.push(mt | value as u8);
        } else if value <= u8::MAX as u64 {
            self.out.push(mt | INFO_1BYTE);
            self.out.push(value as u8);
        } else if value <= u16::MAX as u64 {
            self.out.push(mt | INFO_2BYTES);
            self.out.extend_from_slice(&(value as u16).to_be_bytes());
        } else if value <= u32::MAX as u64 {
            self.out.push(mt | INFO_4BYTES);
            self.out.extend_from_slice(&(value as u32).to_be_bytes());
        } else {
            self.out.push(mt | INFO_8BYTES);
            self.out.extend_from_slice(&value.to_be_bytes());
        }
    }

    #[inline]
    fn write_int(&mut self, v: i64) {
        if v >= 0 {
            self.write_token_start(MAJOR_UNSIGNED, v as u64);
        } else {
            // NEGATIVE encodes -(n+1), so the stored value is -(v) - 1.
            self.write_token_start(MAJOR_NEGATIVE, (-(v + 1)) as u64);
        }
    }

    #[inline]
    fn write_str(&mut self, s: &str) {
        self.write_token_start(MAJOR_STRING, s.len() as u64);
        self.out.extend_from_slice(s.as_bytes());
    }

    /// Begin an envelope: push tag + 32-bit byte-string header with a
    /// placeholder length. Returns the offset of the 4 length bytes so they
    /// can be back-patched once the contents are written.
    fn open_envelope(&mut self) -> usize {
        self.out.push(INITIAL_BYTE_ENVELOPE); // 0xD8
        self.out.push(CBOR_ENVELOPE_TAG); // 0x18 (24)
        self.out.push(INITIAL_BYTE_32BIT_BYTESTRING); // 0x5A
        let pos = self.out.len();
        self.out.extend_from_slice(&[0, 0, 0, 0]);
        pos
    }

    /// Back-patch an envelope length: everything written past the 4 length
    /// bytes is the payload size.
    fn close_envelope(&mut self, len_pos: usize) {
        let payload = (self.out.len() - (len_pos + 4)) as u32;
        self.out[len_pos..len_pos + 4].copy_from_slice(&payload.to_be_bytes());
    }
}

/// Tracks one open compound (map/array) so we can back-patch its envelope and
/// emit the stop byte on `end`.
pub struct Compound<'a> {
    ser: &'a mut Serializer,
    len_pos: usize,
}

impl<'a> Compound<'a> {
    fn finish(self) -> Result<()> {
        self.ser.out.push(STOP_BYTE);
        self.ser.close_envelope(self.len_pos);
        Ok(())
    }
}

impl<'a> ser::Serializer for &'a mut Serializer {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;

    fn serialize_bool(self, v: bool) -> Result<()> {
        self.out.push(if v { ENCODED_TRUE } else { ENCODED_FALSE });
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<()> {
        self.write_int(v as i64);
        Ok(())
    }
    fn serialize_i16(self, v: i16) -> Result<()> {
        self.write_int(v as i64);
        Ok(())
    }
    fn serialize_i32(self, v: i32) -> Result<()> {
        self.write_int(v as i64);
        Ok(())
    }
    fn serialize_i64(self, v: i64) -> Result<()> {
        self.write_int(v);
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<()> {
        self.write_int(v as i64);
        Ok(())
    }
    fn serialize_u16(self, v: u16) -> Result<()> {
        self.write_int(v as i64);
        Ok(())
    }
    fn serialize_u32(self, v: u32) -> Result<()> {
        self.write_int(v as i64);
        Ok(())
    }
    fn serialize_u64(self, v: u64) -> Result<()> {
        // CDP scalars are int32-range; values beyond i64 fall back to double.
        if v <= i64::MAX as u64 {
            self.write_int(v as i64);
        } else {
            self.serialize_f64(v as f64)?;
        }
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<()> {
        self.serialize_f64(v as f64)
    }
    fn serialize_f64(self, v: f64) -> Result<()> {
        self.out.push(INITIAL_BYTE_DOUBLE);
        self.out.extend_from_slice(&v.to_be_bytes());
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<()> {
        self.serialize_str(&v.to_string())
    }
    fn serialize_str(self, v: &str) -> Result<()> {
        self.write_str(v);
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<()> {
        // Binary: tag 22 (expect base64) + definite-length byte string.
        self.out.push(EXPECTED_CONVERSION_TO_BASE64_TAG);
        self.write_token_start(MAJOR_BYTE_STRING, v.len() as u64);
        self.out.extend_from_slice(v);
        Ok(())
    }
    fn serialize_none(self) -> Result<()> {
        self.out.push(ENCODED_NULL);
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<()> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<()> {
        self.out.push(ENCODED_NULL);
        Ok(())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
        self.serialize_unit()
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _idx: u32,
        variant: &'static str,
    ) -> Result<()> {
        // CDP enums serialize as their string name.
        self.serialize_str(variant)
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _idx: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<()> {
        // Externally-tagged: { variant: value } as an enveloped map.
        let len_pos = self.open_envelope();
        self.out.push(INDEF_MAP_START);
        self.write_str(variant);
        value.serialize(&mut *self)?;
        self.out.push(STOP_BYTE);
        self.close_envelope(len_pos);
        Ok(())
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq> {
        let len_pos = self.open_envelope();
        self.out.push(INDEF_ARRAY_START);
        Ok(Compound { ser: self, len_pos })
    }
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _idx: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        // CDP types never use tuple-variant enums; reject rather than guess.
        Err(Error("tuple variant unsupported in crdtp dialect".into()))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap> {
        let len_pos = self.open_envelope();
        self.out.push(INDEF_MAP_START);
        Ok(Compound { ser: self, len_pos })
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct> {
        self.serialize_map(None)
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _idx: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant> {
        Err(Error("struct variant unsupported in crdtp dialect".into()))
    }
}

impl<'a> ser::SerializeSeq for Compound<'a> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut *self.ser)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}
impl<'a> ser::SerializeTuple for Compound<'a> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut *self.ser)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}
impl<'a> ser::SerializeTupleStruct for Compound<'a> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut *self.ser)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}
impl<'a> ser::SerializeTupleVariant for Compound<'a> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _value: &T) -> Result<()> {
        Err(Error("tuple variant unsupported in crdtp dialect".into()))
    }
    fn end(self) -> Result<()> {
        Err(Error("tuple variant unsupported in crdtp dialect".into()))
    }
}
impl<'a> ser::SerializeMap for Compound<'a> {
    type Ok = ();
    type Error = Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<()> {
        key.serialize(&mut *self.ser)
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut *self.ser)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}
impl<'a> ser::SerializeStruct for Compound<'a> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        self.ser.write_str(key);
        value.serialize(&mut *self.ser)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}
impl<'a> ser::SerializeStructVariant for Compound<'a> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        _value: &T,
    ) -> Result<()> {
        Err(Error("struct variant unsupported in crdtp dialect".into()))
    }
    fn end(self) -> Result<()> {
        Err(Error("struct variant unsupported in crdtp dialect".into()))
    }
}
