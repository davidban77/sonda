//! tonic codecs that encode and decode messages with prost.
//!
//! tonic 0.14 no longer ships a prost codec. [`ProstCodec`] serves the OTLP
//! gRPC sink and the gNMI `Capabilities` RPC; [`PreEncodedCodec`] serves the
//! gNMI `Subscribe` RPC, whose responses are already encoded. Compiled with
//! the `otlp` or `gnmi` feature flag; [`PreEncoded`] and its codec with
//! `gnmi` only.

use std::marker::PhantomData;

use bytes::Buf;
#[cfg(feature = "gnmi")]
use bytes::{BufMut, Bytes};
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
        // A zero-length frame is a message with every field at its default.
        let buf = src.copy_to_bytes(src.remaining());
        T::decode(buf)
            .map(Some)
            .map_err(|e| Status::internal(format!("protobuf decode error: {e}")))
    }
}

/// A response payload already encoded as protobuf bytes; the tonic encoder
/// copies it verbatim.
#[cfg(feature = "gnmi")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreEncoded(pub Bytes);

/// A gRPC codec that sends [`PreEncoded`] messages as they are and decodes
/// the messages it receives as `U` with prost.
#[cfg(feature = "gnmi")]
#[derive(Debug, Clone)]
pub(crate) struct PreEncodedCodec<U>(PhantomData<U>);

#[cfg(feature = "gnmi")]
impl<U> Default for PreEncodedCodec<U> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[cfg(feature = "gnmi")]
impl<U> Codec for PreEncodedCodec<U>
where
    U: Message + Default + 'static,
{
    type Encode = PreEncoded;
    type Decode = U;
    type Encoder = PreEncodedEncoder;
    type Decoder = ProstDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        PreEncodedEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        ProstDecoder(PhantomData)
    }
}

/// Writes the bytes of a [`PreEncoded`] message unchanged.
#[cfg(feature = "gnmi")]
#[derive(Debug)]
pub(crate) struct PreEncodedEncoder;

#[cfg(feature = "gnmi")]
impl TonicEncoder for PreEncodedEncoder {
    type Item = PreEncoded;
    type Error = Status;

    fn encode(
        &mut self,
        item: Self::Item,
        dst: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), Self::Error> {
        dst.put_slice(&item.0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::codec::Streaming;

    #[derive(Clone, PartialEq, prost::Message)]
    struct Probe {
        #[prost(string, tag = "1")]
        name: String,
    }

    /// One uncompressed gRPC frame carrying `message`.
    fn frame(message: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8];
        frame.extend_from_slice(&(message.len() as u32).to_be_bytes());
        frame.extend_from_slice(message);
        frame
    }

    /// An empty frame is a message with every field at its default, not
    /// the end of the stream; the stream ends after it.
    #[tokio::test]
    async fn zero_length_frame_decodes_as_default() {
        let named = Probe {
            name: "x".to_string(),
        };
        let mut body = frame(&[]);
        body.extend(frame(&named.encode_to_vec()));
        let body = String::from_utf8(body).expect("these frames are valid UTF-8");
        let mut stream =
            Streaming::<Probe>::new_request(ProstDecoder::<Probe>(PhantomData), body, None, None);

        let limit = std::time::Duration::from_secs(1);
        let first = tokio::time::timeout(limit, stream.message())
            .await
            .expect("no stall")
            .expect("the empty frame decodes");
        assert_eq!(first, Some(Probe::default()));
        let second = tokio::time::timeout(limit, stream.message())
            .await
            .expect("no stall")
            .expect("the next frame decodes");
        assert_eq!(second, Some(named));
        let end = tokio::time::timeout(limit, stream.message())
            .await
            .expect("no stall")
            .expect("clean end");
        assert_eq!(end, None);
    }
}
