//! Protobuf codec: binary (through [`prost`]) and pblite (JSON arrays) from one definition.
//!
//! A message type is declared once with the [`message!`](crate::message) macro, listing each
//! field's number, name and *kind*. A kind is a zero-sized marker type implementing [`Kind`]
//! that knows the Rust representation of the field and how to encode it in both encodings:
//!
//! | Kind | Rust value | binary | pblite |
//! |---|---|---|---|
//! | [`Str`] | `String` | string | string |
//! | [`Bytes`] | `Vec<u8>` | bytes | base64 string |
//! | [`I32`], [`I64`], [`U64`], [`Enum`] | integers | varint | number (decodes strings too) |
//! | [`Fx64`] | `u64` | fixed64 | number |
//! | [`Bool`] | `bool` | varint | `true`/`false` (decodes numbers too) |
//! | [`Msg<M>`] | `Option<M>` | nested message | nested array |
//! | [`B64Msg<M>`] | `Option<M>` | nested message | base64 of the binary form of `M` |
//! | [`B64Str`] | `String` | string | base64 of the UTF-8 bytes |
//! | [`Msgs<M>`] | `Vec<M>` | repeated nested message | array of arrays |
//! | [`Rep<K>`] | `Vec<K::Value>` | repeated scalar (packed accepted on decode) | array |
//! | [`Opt<K>`] | `Option<K::Value>` | explicit presence | as `K` |
//!
//! Unknown fields are skipped on decode in both encodings.

use std::fmt::Debug;
use std::marker::PhantomData;

use base64::Engine as _;
use bytes::{Buf, BufMut};
pub use prost;
use prost::DecodeError;
use prost::encoding::{self as enc, DecodeContext, WireType};
use serde_json::Value as Json;

/// Error decoding the pblite (JSON) encoding.
#[derive(Debug, thiserror::Error)]
pub enum PbLiteError {
    /// The text is not valid JSON.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A value has the wrong JSON type for its field.
    #[error("field {field}: expected {expected}, got {got}")]
    Type {
        /// Field number (0 when unknown).
        field: u32,
        /// Expected JSON type.
        expected: &'static str,
        /// What was found.
        got: String,
    },
    /// A `b64(M)` field or a `bytes` field holds invalid base64.
    #[error("invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    /// A `b64(M)` field holds an invalid binary message.
    #[error("invalid embedded protobuf: {0}")]
    Proto(#[from] DecodeError),
}

impl PbLiteError {
    fn ty(expected: &'static str, got: &Json) -> Self {
        let mut s = got.to_string();
        if s.len() > 40 {
            s.truncate(s.floor_char_boundary(40));
            s.push('…');
        }
        PbLiteError::Type {
            field: 0,
            expected,
            got: s,
        }
    }

    /// Records the field number on a type error.
    pub fn in_field(self, n: u32) -> Self {
        match self {
            PbLiteError::Type {
                field: 0,
                expected,
                got,
            } => PbLiteError::Type {
                field: n,
                expected,
                got,
            },
            e => e,
        }
    }
}

/// Encoding and decoding in the pblite (JSON array) form.
pub trait PbLite: Sized {
    /// Encodes as a pblite JSON value (an array).
    fn to_pblite(&self) -> Json;
    /// Decodes from a pblite JSON value. `null` gives the default message.
    fn from_pblite(json: &Json) -> Result<Self, PbLiteError>;
}

/// Everything a wire message type implements: binary and pblite encodings.
pub trait Proto:
    prost::Message + PbLite + Default + Clone + PartialEq + Debug + Send + Sync + 'static
{
}
impl<T> Proto for T where
    T: prost::Message + PbLite + Default + Clone + PartialEq + Debug + Send + Sync + 'static
{
}

/// Encodes a message in binary protobuf.
pub fn to_binary<M: Proto>(m: &M) -> Vec<u8> {
    m.encode_to_vec()
}

/// Decodes a binary protobuf message.
pub fn from_binary<M: Proto>(b: &[u8]) -> Result<M, DecodeError> {
    M::decode(b)
}

/// Encodes a message as pblite JSON text.
pub fn to_pblite_string<M: Proto>(m: &M) -> String {
    m.to_pblite().to_string()
}

/// Decodes a message from pblite JSON text.
pub fn from_pblite_slice<M: Proto>(b: &[u8]) -> Result<M, PbLiteError> {
    let v: Json = serde_json::from_slice(b)?;
    M::from_pblite(&v)
}

pub(crate) const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Lenient base64 decoding: standard or URL-safe alphabet, padding optional.
pub fn b64_decode_lenient(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::alphabet;
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    const CFG: GeneralPurposeConfig =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    const STD: GeneralPurpose = GeneralPurpose::new(&alphabet::STANDARD, CFG);
    const URL: GeneralPurpose = GeneralPurpose::new(&alphabet::URL_SAFE, CFG);
    let s = s.trim();
    if s.contains(['-', '_']) {
        URL.decode(s)
    } else {
        STD.decode(s)
    }
}

/// A field kind: Rust representation plus both encodings. See the [module docs](self).
pub trait Kind {
    /// The Rust type of the field.
    type Value: Default + Clone + PartialEq + Debug + Send + Sync;
    /// Wire type of one element.
    const WIRE: WireType;
    /// In pblite, an unset value is written `[]` instead of `null` (repeated fields).
    const EMPTY_AS_ARRAY: bool = false;
    /// Whether the value is unset (not encoded).
    fn is_unset(v: &Self::Value) -> bool;
    /// Encodes key and value, unconditionally.
    fn encode(tag: u32, v: &Self::Value, buf: &mut impl BufMut);
    /// Length of [`Kind::encode`]'s output.
    fn encoded_len(tag: u32, v: &Self::Value) -> usize;
    /// Merges one occurrence from the wire.
    fn merge<B: Buf>(
        wt: WireType,
        v: &mut Self::Value,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError>;
    /// pblite encoding of a set value.
    fn to_json(v: &Self::Value) -> Json;
    /// pblite decoding of a non-null value.
    fn from_json(j: &Json) -> Result<Self::Value, PbLiteError>;
}

fn json_i64(j: &Json) -> Result<i64, PbLiteError> {
    match j {
        Json::Number(n) => n
            .as_i64()
            .or_else(|| n.as_u64().map(|u| u as i64))
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
            .ok_or_else(|| PbLiteError::ty("integer", j)),
        Json::String(s) => s
            .trim()
            .parse::<i64>()
            .or_else(|_| s.trim().parse::<u64>().map(|u| u as i64))
            .map_err(|_| PbLiteError::ty("integer", j)),
        Json::Bool(b) => Ok(*b as i64),
        _ => Err(PbLiteError::ty("integer", j)),
    }
}

fn json_u64(j: &Json) -> Result<u64, PbLiteError> {
    match j {
        Json::Number(n) => n
            .as_u64()
            .or_else(|| n.as_i64().map(|i| i as u64))
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as u64))
            .ok_or_else(|| PbLiteError::ty("integer", j)),
        Json::String(s) => s
            .trim()
            .parse::<u64>()
            .or_else(|_| s.trim().parse::<i64>().map(|i| i as u64))
            .map_err(|_| PbLiteError::ty("integer", j)),
        Json::Bool(b) => Ok(*b as u64),
        _ => Err(PbLiteError::ty("integer", j)),
    }
}

fn json_str(j: &Json) -> Result<&str, PbLiteError> {
    j.as_str().ok_or_else(|| PbLiteError::ty("string", j))
}

macro_rules! scalar_kind {
    ($(#[$m:meta])* $name:ident, $ty:ty, $module:ident, $wire:expr, $to:expr, $from:expr) => {
        $(#[$m])*
        #[derive(Debug)]
        pub struct $name;
        impl Kind for $name {
            type Value = $ty;
            const WIRE: WireType = $wire;
            fn is_unset(v: &$ty) -> bool {
                *v == <$ty>::default()
            }
            fn encode(tag: u32, v: &$ty, buf: &mut impl BufMut) {
                enc::$module::encode(tag, v, buf)
            }
            fn encoded_len(tag: u32, v: &$ty) -> usize {
                enc::$module::encoded_len(tag, v)
            }
            fn merge<B: Buf>(
                wt: WireType,
                v: &mut $ty,
                buf: &mut B,
                ctx: DecodeContext,
            ) -> Result<(), DecodeError> {
                enc::$module::merge(wt, v, buf, ctx)
            }
            fn to_json(v: &$ty) -> Json {
                #[allow(clippy::redundant_closure_call)]
                ($to)(v)
            }
            fn from_json(j: &Json) -> Result<$ty, PbLiteError> {
                #[allow(clippy::redundant_closure_call)]
                ($from)(j)
            }
        }
    };
}

scalar_kind!(
    /// `i32` varint.
    I32, i32, int32, WireType::Varint,
    |v: &i32| Json::from(*v),
    |j: &Json| json_i64(j).map(|v| v as i32)
);
scalar_kind!(
    /// Enumeration value, kept raw as `i32` (unknown values decode fine).
    Enum, i32, int32, WireType::Varint,
    |v: &i32| Json::from(*v),
    |j: &Json| json_i64(j).map(|v| v as i32)
);
scalar_kind!(
    /// `i64` varint.
    I64, i64, int64, WireType::Varint,
    |v: &i64| Json::from(*v),
    json_i64
);
scalar_kind!(
    /// `u64` varint.
    U64, u64, uint64, WireType::Varint,
    |v: &u64| Json::from(*v),
    json_u64
);
scalar_kind!(
    /// `fixed64`.
    Fx64, u64, fixed64, WireType::SixtyFourBit,
    |v: &u64| Json::from(*v),
    json_u64
);
scalar_kind!(
    /// Boolean.
    Bool, bool, bool, WireType::Varint,
    |v: &bool| Json::Bool(*v),
    |j: &Json| match j {
        Json::Bool(b) => Ok(*b),
        Json::Number(_) | Json::String(_) => json_i64(j).map(|v| v != 0),
        _ => Err(PbLiteError::ty("bool", j)),
    }
);
scalar_kind!(
    /// UTF-8 string.
    Str, String, string, WireType::LengthDelimited,
    |v: &String| Json::String(v.clone()),
    |j: &Json| match j {
        Json::Number(n) => Ok(n.to_string()),
        _ => json_str(j).map(str::to_owned),
    }
);
scalar_kind!(
    /// Byte string; base64 in pblite.
    Bytes, Vec<u8>, bytes, WireType::LengthDelimited,
    |v: &Vec<u8>| Json::String(B64.encode(v)),
    |j: &Json| Ok(b64_decode_lenient(json_str(j)?)?)
);

/// A string carried as base64 of its UTF-8 bytes in pblite (`b64(str)`), plain in binary.
#[derive(Debug)]
pub struct B64Str;
impl Kind for B64Str {
    type Value = String;
    const WIRE: WireType = WireType::LengthDelimited;
    fn is_unset(v: &String) -> bool {
        v.is_empty()
    }
    fn encode(tag: u32, v: &String, buf: &mut impl BufMut) {
        enc::string::encode(tag, v, buf)
    }
    fn encoded_len(tag: u32, v: &String) -> usize {
        enc::string::encoded_len(tag, v)
    }
    fn merge<B: Buf>(
        wt: WireType,
        v: &mut String,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        enc::string::merge(wt, v, buf, ctx)
    }
    fn to_json(v: &String) -> Json {
        Json::String(B64.encode(v.as_bytes()))
    }
    fn from_json(j: &Json) -> Result<String, PbLiteError> {
        let raw = b64_decode_lenient(json_str(j)?)?;
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }
}

/// A nested message (with presence: `None` is unset, `Some(default)` is `[]`).
#[derive(Debug)]
pub struct Msg<M>(PhantomData<M>);
impl<M: Proto> Kind for Msg<M> {
    type Value = Option<M>;
    const WIRE: WireType = WireType::LengthDelimited;
    fn is_unset(v: &Option<M>) -> bool {
        v.is_none()
    }
    fn encode(tag: u32, v: &Option<M>, buf: &mut impl BufMut) {
        if let Some(m) = v {
            enc::message::encode(tag, m, buf)
        }
    }
    fn encoded_len(tag: u32, v: &Option<M>) -> usize {
        v.as_ref().map_or(0, |m| enc::message::encoded_len(tag, m))
    }
    fn merge<B: Buf>(
        wt: WireType,
        v: &mut Option<M>,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        enc::message::merge(wt, v.get_or_insert_with(M::default), buf, ctx)
    }
    fn to_json(v: &Option<M>) -> Json {
        v.as_ref().map_or(Json::Null, PbLite::to_pblite)
    }
    fn from_json(j: &Json) -> Result<Option<M>, PbLiteError> {
        M::from_pblite(j).map(Some)
    }
}

/// A nested message, carried in pblite as base64 of its binary encoding (`b64(M)`).
#[derive(Debug)]
pub struct B64Msg<M>(PhantomData<M>);
impl<M: Proto> Kind for B64Msg<M> {
    type Value = Option<M>;
    const WIRE: WireType = WireType::LengthDelimited;
    fn is_unset(v: &Option<M>) -> bool {
        v.is_none()
    }
    fn encode(tag: u32, v: &Option<M>, buf: &mut impl BufMut) {
        Msg::<M>::encode(tag, v, buf)
    }
    fn encoded_len(tag: u32, v: &Option<M>) -> usize {
        Msg::<M>::encoded_len(tag, v)
    }
    fn merge<B: Buf>(
        wt: WireType,
        v: &mut Option<M>,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        Msg::<M>::merge(wt, v, buf, ctx)
    }
    fn to_json(v: &Option<M>) -> Json {
        v.as_ref()
            .map_or(Json::Null, |m| Json::String(B64.encode(m.encode_to_vec())))
    }
    fn from_json(j: &Json) -> Result<Option<M>, PbLiteError> {
        let raw = b64_decode_lenient(json_str(j)?)?;
        Ok(Some(M::decode(raw.as_slice())?))
    }
}

/// Repeated nested message.
#[derive(Debug)]
pub struct Msgs<M>(PhantomData<M>);
impl<M: Proto> Kind for Msgs<M> {
    type Value = Vec<M>;
    const WIRE: WireType = WireType::LengthDelimited;
    const EMPTY_AS_ARRAY: bool = true;
    fn is_unset(v: &Vec<M>) -> bool {
        v.is_empty()
    }
    fn encode(tag: u32, v: &Vec<M>, buf: &mut impl BufMut) {
        enc::message::encode_repeated(tag, v, buf)
    }
    fn encoded_len(tag: u32, v: &Vec<M>) -> usize {
        enc::message::encoded_len_repeated(tag, v)
    }
    fn merge<B: Buf>(
        wt: WireType,
        v: &mut Vec<M>,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        let mut m = M::default();
        enc::message::merge(wt, &mut m, buf, ctx)?;
        v.push(m);
        Ok(())
    }
    fn to_json(v: &Vec<M>) -> Json {
        Json::Array(v.iter().map(PbLite::to_pblite).collect())
    }
    fn from_json(j: &Json) -> Result<Vec<M>, PbLiteError> {
        let arr = j.as_array().ok_or_else(|| PbLiteError::ty("array", j))?;
        arr.iter().map(M::from_pblite).collect()
    }
}

/// Repeated field. Encoded unpacked; packed scalars are accepted on decode.
#[derive(Debug)]
pub struct Rep<K>(PhantomData<K>);
impl<K: Kind> Kind for Rep<K>
where
    K::Value: Default,
{
    type Value = Vec<K::Value>;
    const WIRE: WireType = K::WIRE;
    const EMPTY_AS_ARRAY: bool = true;
    fn is_unset(v: &Vec<K::Value>) -> bool {
        v.is_empty()
    }
    fn encode(tag: u32, v: &Vec<K::Value>, buf: &mut impl BufMut) {
        for e in v {
            K::encode(tag, e, buf)
        }
    }
    fn encoded_len(tag: u32, v: &Vec<K::Value>) -> usize {
        v.iter().map(|e| K::encoded_len(tag, e)).sum()
    }
    fn merge<B: Buf>(
        wt: WireType,
        v: &mut Vec<K::Value>,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        if wt == WireType::LengthDelimited && K::WIRE != WireType::LengthDelimited {
            enc::merge_loop(v, buf, ctx, |v, buf, ctx| {
                let mut e = K::Value::default();
                K::merge(K::WIRE, &mut e, buf, ctx)?;
                v.push(e);
                Ok(())
            })
        } else {
            let mut e = K::Value::default();
            K::merge(wt, &mut e, buf, ctx)?;
            v.push(e);
            Ok(())
        }
    }
    fn to_json(v: &Vec<K::Value>) -> Json {
        Json::Array(v.iter().map(K::to_json).collect())
    }
    fn from_json(j: &Json) -> Result<Vec<K::Value>, PbLiteError> {
        let arr = j.as_array().ok_or_else(|| PbLiteError::ty("array", j))?;
        arr.iter()
            .map(|e| {
                if e.is_null() {
                    Ok(K::Value::default())
                } else {
                    K::from_json(e)
                }
            })
            .collect()
    }
}

/// Scalar with explicit presence (`opt T`): `Some(0)` is encoded, `None` is not.
#[derive(Debug)]
pub struct Opt<K>(PhantomData<K>);
impl<K: Kind> Kind for Opt<K> {
    type Value = Option<K::Value>;
    const WIRE: WireType = K::WIRE;
    fn is_unset(v: &Option<K::Value>) -> bool {
        v.is_none()
    }
    fn encode(tag: u32, v: &Option<K::Value>, buf: &mut impl BufMut) {
        if let Some(x) = v {
            K::encode(tag, x, buf)
        }
    }
    fn encoded_len(tag: u32, v: &Option<K::Value>) -> usize {
        v.as_ref().map_or(0, |x| K::encoded_len(tag, x))
    }
    fn merge<B: Buf>(
        wt: WireType,
        v: &mut Option<K::Value>,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        K::merge(wt, v.get_or_insert_with(Default::default), buf, ctx)
    }
    fn to_json(v: &Option<K::Value>) -> Json {
        v.as_ref().map_or(Json::Null, K::to_json)
    }
    fn from_json(j: &Json) -> Result<Option<K::Value>, PbLiteError> {
        K::from_json(j).map(Some)
    }
}

/// Looks up field `n` in a pblite array. A trailing JSON object, if any, is read as a map
/// of field numbers (a form some pblite encoders use for high field numbers).
#[doc(hidden)]
pub fn pblite_slot(arr: &[Json], n: usize) -> Option<&Json> {
    let v = match arr.get(n - 1) {
        Some(Json::Object(_)) if n == arr.len() => None,
        Some(v) => Some(v),
        None => None,
    };
    match v {
        Some(Json::Null) | None => {
            if let Some(Json::Object(map)) = arr.last() {
                map.get(&n.to_string()).filter(|v| !v.is_null())
            } else {
                None
            }
        }
        v => v,
    }
}

#[doc(hidden)]
pub fn pblite_array(j: &Json) -> Result<Option<&Vec<Json>>, PbLiteError> {
    match j {
        Json::Null => Ok(None),
        Json::Array(a) => Ok(Some(a)),
        other => Err(PbLiteError::ty("array", other)),
    }
}

#[doc(hidden)]
pub use enc::skip_field as __skip_field;
#[doc(hidden)]
pub use prost::encoding::{DecodeContext as __Ctx, WireType as __Wire};

/// Declares a wire message type with binary (prost) and pblite encodings.
///
/// ```ignore
/// message! {
///     /// A party known to the relay.
///     pub struct Device {
///         1 => user_id: I64,
///         2 => source_id: Str,
///         3 => network: Str,
///     }
/// }
/// ```
#[macro_export]
macro_rules! message {
    ($(
        $(#[$m:meta])*
        pub struct $name:ident {
            $( $(#[$fm:meta])* $num:literal => $field:ident : $kind:ty ),* $(,)?
        }
    )*) => {$(
        $(#[$m])*
        #[derive(Clone, PartialEq, Default, Debug)]
        pub struct $name {
            $( $(#[$fm])* pub $field: <$kind as $crate::codec::Kind>::Value, )*
        }

        impl $name {
            /// Highest field number known for this message (pblite array length).
            pub const MAX_FIELD: usize = {
                #[allow(unused_mut)]
                let mut m = 0usize;
                $( if $num > m { m = $num; } )*
                m
            };
        }

        impl $crate::codec::prost::Message for $name {
            #[allow(unused_variables)]
            fn encode_raw(&self, buf: &mut impl $crate::codec::prost::bytes::BufMut) {
                $(
                    if !<$kind as $crate::codec::Kind>::is_unset(&self.$field) {
                        <$kind as $crate::codec::Kind>::encode($num, &self.$field, buf);
                    }
                )*
            }
            fn merge_field(
                &mut self,
                tag: u32,
                wire_type: $crate::codec::__Wire,
                buf: &mut impl $crate::codec::prost::bytes::Buf,
                ctx: $crate::codec::__Ctx,
            ) -> ::std::result::Result<(), $crate::codec::prost::DecodeError> {
                match tag {
                    $( $num => <$kind as $crate::codec::Kind>::merge(wire_type, &mut self.$field, buf, ctx), )*
                    _ => $crate::codec::__skip_field(wire_type, tag, buf, ctx),
                }
            }
            fn encoded_len(&self) -> usize {
                0 $( + if <$kind as $crate::codec::Kind>::is_unset(&self.$field) { 0 } else {
                    <$kind as $crate::codec::Kind>::encoded_len($num, &self.$field)
                } )*
            }
            fn clear(&mut self) {
                *self = Self::default();
            }
        }

        impl $crate::codec::PbLite for $name {
            fn to_pblite(&self) -> ::serde_json::Value {
                #[allow(unused_mut)]
                let mut arr = vec![::serde_json::Value::Null; Self::MAX_FIELD];
                $(
                    if !<$kind as $crate::codec::Kind>::is_unset(&self.$field) {
                        arr[$num - 1] = <$kind as $crate::codec::Kind>::to_json(&self.$field);
                    } else if <$kind as $crate::codec::Kind>::EMPTY_AS_ARRAY {
                        arr[$num - 1] = ::serde_json::Value::Array(Vec::new());
                    }
                )*
                ::serde_json::Value::Array(arr)
            }
            #[allow(unused_variables)]
            fn from_pblite(json: &::serde_json::Value) -> ::std::result::Result<Self, $crate::codec::PbLiteError> {
                #[allow(unused_mut)]
                let mut out = Self::default();
                let Some(arr) = $crate::codec::pblite_array(json)? else { return Ok(out) };
                $(
                    if let Some(v) = $crate::codec::pblite_slot(arr, $num) {
                        out.$field = <$kind as $crate::codec::Kind>::from_json(v)
                            .map_err(|e| e.in_field($num))?;
                    }
                )*
                Ok(out)
            }
        }
    )*};
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire;
    use serde_json::json;

    #[test]
    fn type_error_truncates_multibyte_text_at_char_boundary() {
        let value = Json::String("é".repeat(21));
        let PbLiteError::Type { got, .. } = PbLiteError::ty("array", &value) else {
            panic!("expected a type error");
        };
        let prefix = got.strip_suffix('…').expect("truncated error");
        assert!(prefix.len() <= 40);
        assert!(std::str::from_utf8(prefix.as_bytes()).is_ok());
    }

    crate::message! {
        /// Test message.
        pub struct Inner {
            2 => n: I32,
        }
        /// Test message covering every kind.
        pub struct Sample {
            1 => s: Str,
            2 => i: I64,
            3 => inner: Msg<Inner>,
            4 => b: Bytes,
            5 => flag: Bool,
            6 => list: Rep<Str>,
            7 => nums: Rep<U64>,
            8 => msgs: Msgs<Inner>,
            9 => emb: B64Msg<Inner>,
            10 => emb_str: B64Str,
            11 => opt: Opt<I32>,
            12 => e: Enum,
            13 => f: Fx64,
        }
    }

    #[test]
    fn spec_example() {
        // field 1 = "a", field 3 = {2: 5}, highest field 4
        crate::message! {
            pub struct Ex { 1 => a: Str, 3 => m: Msg<Inner>, 4 => z: I32 }
        }
        let m = Ex {
            a: "a".into(),
            m: Some(Inner { n: 5 }),
            z: 0,
        };
        assert_eq!(m.to_pblite(), json!(["a", null, [null, 5], null]));
        assert_eq!(
            Ex::from_pblite(&json!(["a", null, [null, 5], null])).unwrap(),
            m
        );
    }

    #[test]
    fn nulls_and_defaults() {
        let m = Sample::default();
        let j = m.to_pblite();
        let a = j.as_array().unwrap();
        assert_eq!(a.len(), 13);
        // repeated fields are [] when empty, everything else null
        for (i, v) in a.iter().enumerate() {
            if [5, 6, 7].contains(&i) {
                assert_eq!(v, &json!([]), "index {i}");
            } else {
                assert!(v.is_null(), "index {i}");
            }
        }
        assert_eq!(Sample::from_pblite(&json!(null)).unwrap(), m);
        assert_eq!(Sample::from_pblite(&json!([])).unwrap(), m);
        // a present message with all fields unset keeps its full size; one without
        // fields is []
        let m = Sample {
            inner: Some(Inner::default()),
            ..Default::default()
        };
        assert_eq!(m.to_pblite()[2], json!([null, null]));
        assert_eq!(wire::Empty {}.to_pblite(), json!([]));
        assert_eq!(Sample::from_pblite(&m.to_pblite()).unwrap(), m);
    }

    fn full() -> Sample {
        Sample {
            s: "héllo".into(),
            i: -9_007_199_254_740_993,
            inner: Some(Inner { n: -3 }),
            b: vec![0, 1, 2, 250],
            flag: true,
            list: vec!["x".into(), String::new(), "z".into()],
            nums: vec![1, u64::MAX],
            msgs: vec![Inner { n: 1 }, Inner::default()],
            emb: Some(Inner { n: 7 }),
            emb_str: "uuid-1".into(),
            opt: Some(0),
            e: 42,
            f: 0xdead_beef_0000_0001,
        }
    }

    #[test]
    fn round_trips() {
        let m = full();
        let bin = to_binary(&m);
        assert_eq!(from_binary::<Sample>(&bin).unwrap(), m);
        let txt = to_pblite_string(&m);
        assert_eq!(from_pblite_slice::<Sample>(txt.as_bytes()).unwrap(), m);
        let j = m.to_pblite();
        assert_eq!(j[3], json!("AAEC+g=="));
        // b64(M): base64 of the binary form
        assert_eq!(j[8], json!(B64.encode(to_binary(&Inner { n: 7 }))));
        assert_eq!(j[9], json!(B64.encode("uuid-1")));
        // opt: explicit zero is present
        assert_eq!(j[10], json!(0));
        assert_eq!(j[7], json!([[null, 1], [null, null]]));
    }

    #[test]
    fn lenient_decoding() {
        let j = json!([
            7,                      // number for a string field
            "-12",                  // i64 as a string
            [null, "5"],            // i32 as a string
            "AAEC-g",               // url-safe base64 without padding
            1,                      // bool as a number
            ["a", null],            // null element of a repeated string
            ["18446744073709551615", 3],
            [[null, 1], null],      // null element of a repeated message
            null,
            null,
            null,
            "3",
            "12",
            "unknown", {"x": 1}     // unknown indices ignored
        ]);
        let m = Sample::from_pblite(&j).unwrap();
        assert_eq!(m.s, "7");
        assert_eq!(m.i, -12);
        assert_eq!(m.inner, Some(Inner { n: 5 }));
        assert_eq!(m.b, vec![0, 1, 2, 250]);
        assert!(m.flag);
        assert_eq!(m.list, vec!["a".to_string(), String::new()]);
        assert_eq!(m.nums, vec![u64::MAX, 3]);
        assert_eq!(m.msgs, vec![Inner { n: 1 }, Inner::default()]);
        assert_eq!(m.e, 3);
        assert_eq!(m.f, 12);
        // type errors carry the field number
        let e = Sample::from_pblite(&json!([[1]])).unwrap_err();
        assert!(e.to_string().contains("field 1"), "{e}");
    }

    #[test]
    fn trailing_object_extension() {
        let j = json!(["a", {"12": 9}]);
        let m = Sample::from_pblite(&j).unwrap();
        assert_eq!(m.s, "a");
        assert_eq!(m.e, 9);
    }

    #[test]
    fn unknown_binary_fields_and_packed() {
        // field 99 (varint), field 100 (bytes), then field 7 packed [1, 300]
        let mut buf = Vec::new();
        prost::encoding::encode_key(99, WireType::Varint, &mut buf);
        prost::encoding::encode_varint(5, &mut buf);
        prost::encoding::encode_key(100, WireType::LengthDelimited, &mut buf);
        prost::encoding::encode_varint(2, &mut buf);
        buf.extend_from_slice(b"hi");
        prost::encoding::encode_key(7, WireType::LengthDelimited, &mut buf);
        prost::encoding::encode_varint(3, &mut buf);
        buf.extend_from_slice(&[1, 0xac, 0x02]);
        let m: Sample = from_binary(&buf).unwrap();
        assert_eq!(m.nums, vec![1, 300]);
    }

    #[test]
    fn wire_shapes() {
        // ReceiveMessages field 4 is always [null, []]
        let r = wire::ReceiveMessagesRequest {
            credentials: None,
            unknown4: Some(wire::ReceiveUnknown {
                empty: Some(wire::Empty {}),
            }),
        };
        assert_eq!(r.to_pblite(), json!([null, null, null, [null, []]]));
        // QR session destinations: []
        let s = wire::SendMessageRequest::default();
        assert_eq!(s.to_pblite()[8], json!([]));
        assert_eq!(wire::ClientVersion::MAX_FIELD, 9);
        assert_eq!(wire::PushOptions::MAX_FIELD, 102);
    }

    #[test]
    fn stream_item_decoding() {
        let data = wire::DataPayload {
            request_id: "r1".into(),
            action: 16,
            encrypted: vec![1, 2, 3],
            ..Default::default()
        };
        let payload = B64.encode(to_binary(&data));
        let txt = format!(
            r#"[null,["id-9",19,"123",null,2,"456",null,[1,"src","Bugle"],null,null,null,"{payload}"]]"#
        );
        let item: wire::StreamItem = from_pblite_slice(txt.as_bytes()).unwrap();
        let env = item.envelope.unwrap();
        assert_eq!(env.id, "id-9");
        assert_eq!(env.route, 19);
        assert_eq!(env.exec_start, 123);
        assert_eq!(env.phone.unwrap().source_id, "src");
        let d: wire::DataPayload = from_binary(&env.payload).unwrap();
        assert_eq!(d, data);
        let hb: wire::StreamItem = from_pblite_slice(b"[null,null,[]]").unwrap();
        assert!(hb.heartbeat.is_some() && hb.envelope.is_none());
        let bl: wire::StreamItem = from_pblite_slice(b"[null,null,null,[4]]").unwrap();
        assert_eq!(bl.backlog.unwrap().count, Some(4));
    }
}
