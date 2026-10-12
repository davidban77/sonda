//! A tonic codec that encodes and decodes messages with prost.
//!
//! tonic 0.14 no longer ships a prost codec, so the gRPC sinks and the gNMI
//! target share this one. Requires the `otlp` or `gnmi` feature flag.

use std::marker::PhantomData;

use bytes::Buf;
use prost::Message;
use tonic::codec::{Codec, Decoder, Encoder as TonicEncoder};
use tonic::Status;

/// A gRPC codec that uses prost for protobuf encoding and decoding.
///
/// `T` is the message type this side sends and `U` the message type it
/// receives: request and response for a client, response and request for a
/// server.
#[derive(Debug, Clone)]
pub(crate) struct ProstCodec<T, U>(PhantomData<(T, U)>);

impl<T, U> Default for ProstCodec<T, U> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T, U> Codec for ProstCodec<T, U>
where
    T: Message + 'static,
    U: Message + Default + 'static,
{
    type Encode = T;
    type Decode = U;
    type Encoder = ProstEncoder<T>;
    type Decoder = ProstDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        ProstEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        ProstDecoder(PhantomData)
    }
}

/// Prost-based encoder for outbound gRPC messages.
#[derive(Debug)]
pub(crate) struct ProstEncoder<T>(PhantomData<T>);

impl<T: Message + 'static> TonicEncoder for ProstEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(
        &mut self,
        item: Self::Item,
        dst: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), Self::Error> {
        item.encode(dst)
            .map_err(|e| Status::internal(format!("protobuf encode error: {e}")))
    }
}

/// Prost-based decoder for inbound gRPC messages.
#[derive(Debug)]
pub(crate) struct ProstDecoder<T>(PhantomData<T>);

impl<T: Message + Default + 'static> Decoder for ProstDecoder<T> {
    type Item = T;
    type Error = Status;

    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<Self::Item>, Self::Error> {
        let buf = src.copy_to_bytes(src.remaining());
        if buf.is_empty() {
            return Ok(None);
        }
        T::decode(buf)
            .map(Some)
            .map_err(|e| Status::internal(format!("protobuf decode error: {e}")))
    }
}
