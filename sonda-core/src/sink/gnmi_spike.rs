//! Throwaway spike: a bidirectional-streaming gRPC service served by tonic
//! with hand-written prost messages and no generated code.
//!
//! The module serves a single RPC, `/gnmi.gNMI/Subscribe`, over a listener
//! bound to an ephemeral port. A client opens the stream, sends one
//! `SubscribeRequest`, and receives a `Notification`, then
//! `sync_response: true`, then one `Notification` per second until it closes
//! the request stream or the server's cancellation token fires.
//!
//! Only the message fields the round trip needs are declared; the field
//! numbers are those of `openconfig/gnmi` `proto/gnmi/gnmi.proto`.
//!
//! Requires the `gnmi-spike` feature flag.

use std::convert::Infallible;
use std::future::Future;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Buf;
use prost::Message;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tokio_util::sync::CancellationToken;
use tonic::codec::{Codec, Decoder, Encoder as TonicEncoder};
use tonic::server::NamedService;
use tonic::{Status, Streaming};
use tower::Service;

use crate::SondaError;

/// The gRPC service name this spike routes for.
pub const SERVICE_NAME: &str = "gnmi.gNMI";

/// The full path of the `Subscribe` RPC.
pub const SUBSCRIBE_PATH: &str = "/gnmi.gNMI/Subscribe";

/// Interval between the notifications that follow the initial sync.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Capacity of the outbound response channel.
const OUTBOUND_CAPACITY: usize = 8;

// ---------------------------------------------------------------------------
// Protobuf message subset
// ---------------------------------------------------------------------------

/// A client's subscription request.
///
/// Corresponds to `gnmi.SubscribeRequest`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SubscribeRequest {
    /// The subscription list carried by the first message of the stream.
    #[prost(message, optional, tag = "1")]
    pub subscribe: Option<SubscriptionList>,
}

/// The set of subscriptions a client asks for.
///
/// Corresponds to `gnmi.SubscriptionList`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SubscriptionList {
    /// The individual subscriptions.
    #[prost(message, repeated, tag = "2")]
    pub subscription: Vec<Subscription>,
}

/// One subscription within a [`SubscriptionList`].
///
/// Corresponds to `gnmi.Subscription`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Subscription {
    /// The path this subscription selects.
    #[prost(message, optional, tag = "1")]
    pub path: Option<Path>,
}

/// A path into the target's data tree.
///
/// Corresponds to `gnmi.Path`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Path {
    /// The path elements, root first.
    #[prost(message, repeated, tag = "3")]
    pub elem: Vec<PathElem>,
}

/// One element of a [`Path`].
///
/// Corresponds to `gnmi.PathElem`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct PathElem {
    /// The element name.
    #[prost(string, tag = "1")]
    pub name: String,
}

/// A server's response on the `Subscribe` stream.
///
/// Corresponds to `gnmi.SubscribeResponse`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SubscribeResponse {
    /// The response payload, represented as a oneof.
    #[prost(oneof = "subscribe_response::Response", tags = "1, 3")]
    pub response: Option<subscribe_response::Response>,
}

/// Inner oneof variants for [`SubscribeResponse`].
pub mod subscribe_response {
    /// The response payload variants.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Response {
        /// A notification carrying data.
        #[prost(message, tag = "1")]
        Update(super::Notification),
        /// The end-of-initial-sync marker.
        #[prost(bool, tag = "3")]
        SyncResponse(bool),
    }
}

/// A set of updates sharing one timestamp.
///
/// Corresponds to `gnmi.Notification`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Notification {
    /// Nanoseconds since the Unix epoch.
    #[prost(int64, tag = "1")]
    pub timestamp: i64,
}

// ---------------------------------------------------------------------------
// Codec
// ---------------------------------------------------------------------------

/// A gRPC codec that uses prost for protobuf encoding and decoding.
///
/// Type parameters `T` and `U` are the outbound and inbound message types.
#[derive(Debug, Clone)]
struct SpikeCodec<T, U>(PhantomData<(T, U)>);

impl<T, U> Default for SpikeCodec<T, U> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T, U> Codec for SpikeCodec<T, U>
where
    T: Message + 'static,
    U: Message + Default + 'static,
{
    type Encode = T;
    type Decode = U;
    type Encoder = SpikeProstEncoder<T>;
    type Decoder = SpikeProstDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        SpikeProstEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        SpikeProstDecoder(PhantomData)
    }
}

/// Prost-based encoder for outbound gRPC messages.
#[derive(Debug)]
struct SpikeProstEncoder<T>(PhantomData<T>);

impl<T: Message + 'static> TonicEncoder for SpikeProstEncoder<T> {
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
struct SpikeProstDecoder<T>(PhantomData<T>);

impl<T: Message + Default + 'static> Decoder for SpikeProstDecoder<T> {
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

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// The `Subscribe` RPC handler.
///
/// Implements [`tower::Service`] over `tonic::Request<Streaming<SubscribeRequest>>`,
/// which is the shape `tonic::server::StreamingService` is blanket-implemented for.
#[derive(Clone)]
struct SubscribeHandler;

/// The response stream type the handler produces.
type SubscribeStream = ReceiverStream<Result<SubscribeResponse, Status>>;

/// A boxed future returning `Result<T, E>`.
type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;

impl Service<tonic::Request<Streaming<SubscribeRequest>>> for SubscribeHandler {
    type Response = tonic::Response<SubscribeStream>;
    type Error = Status;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: tonic::Request<Streaming<SubscribeRequest>>) -> Self::Future {
        Box::pin(async move {
            let mut inbound = request.into_inner();
            if inbound.message().await?.is_none() {
                return Err(Status::invalid_argument(
                    "subscribe stream closed before the first request",
                ));
            }

            let (tx, rx) = mpsc::channel(OUTBOUND_CAPACITY);

            // A separate task owns the inbound half so the producer below can
            // wait on a cancel-safe oneshot instead of polling `Streaming`.
            let (closed_tx, mut closed_rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                while matches!(inbound.message().await, Ok(Some(_))) {}
                let _ = closed_tx.send(());
            });

            tokio::spawn(async move {
                if tx.send(Ok(notification_now())).await.is_err() {
                    return;
                }
                if tx.send(Ok(sync_response())).await.is_err() {
                    return;
                }
                let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
                // `interval` fires immediately on its first tick; drop it so
                // the cadence after the sync marker is one per SAMPLE_INTERVAL.
                ticker.tick().await;
                loop {
                    tokio::select! {
                        _ = &mut closed_rx => return,
                        _ = ticker.tick() => {
                            if tx.send(Ok(notification_now())).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });

            Ok(tonic::Response::new(ReceiverStream::new(rx)))
        })
    }
}

/// A `SubscribeResponse` carrying a `Notification` stamped with the current time.
fn notification_now() -> SubscribeResponse {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    SubscribeResponse {
        response: Some(subscribe_response::Response::Update(Notification {
            timestamp,
        })),
    }
}

/// A `SubscribeResponse` carrying the `sync_response: true` marker.
fn sync_response() -> SubscribeResponse {
    SubscribeResponse {
        response: Some(subscribe_response::Response::SyncResponse(true)),
    }
}

// ---------------------------------------------------------------------------
// Routing shim
// ---------------------------------------------------------------------------

/// Routes HTTP/2 requests by path to the gNMI RPCs.
///
/// This is the shape generated server code expands to: a `Clone` service over
/// `http::Request<tonic::body::Body>` that matches `req.uri().path()` and
/// dispatches to [`tonic::server::Grpc`]. Any unrecognised path answers
/// `Unimplemented`.
#[derive(Clone, Debug, Default)]
pub struct GnmiSpikeService;

impl NamedService for GnmiSpikeService {
    const NAME: &'static str = SERVICE_NAME;
}

impl Service<http::Request<tonic::body::Body>> for GnmiSpikeService {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<tonic::body::Body>) -> Self::Future {
        match req.uri().path() {
            SUBSCRIBE_PATH => Box::pin(async move {
                let mut grpc = tonic::server::Grpc::new(SpikeCodec::<
                    SubscribeResponse,
                    SubscribeRequest,
                >::default());
                Ok(grpc.streaming(SubscribeHandler, req).await)
            }),
            other => {
                let status = Status::unimplemented(format!("unknown method {other}"));
                Box::pin(async move { Ok(status.into_http()) })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// A running spike server and the address it bound.
#[derive(Debug)]
pub struct SpikeServer {
    /// The address the listener bound, with the ephemeral port resolved.
    local_addr: SocketAddr,
    /// The task driving the tonic server.
    handle: JoinHandle<()>,
}

impl SpikeServer {
    /// The address the listener bound.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Wait for the server task to finish after its token was cancelled.
    pub async fn wait(self) {
        let _ = self.handle.await;
    }
}

/// Bind `listen` and serve [`GnmiSpikeService`] until `token` is cancelled.
///
/// Pass `127.0.0.1:0` to bind an ephemeral port and read it back from
/// [`SpikeServer::local_addr`].
///
/// # Errors
///
/// Returns [`SondaError::Sink`] if the address cannot be bound.
pub async fn serve(
    listen: SocketAddr,
    token: CancellationToken,
) -> Result<SpikeServer, SondaError> {
    let listener = TcpListener::bind(listen).await.map_err(SondaError::Sink)?;
    let local_addr = listener.local_addr().map_err(SondaError::Sink)?;
    let incoming = TcpListenerStream::new(listener);

    let handle = tokio::spawn(async move {
        let result = tonic::transport::Server::builder()
            .serve_with_incoming_shutdown(GnmiSpikeService, incoming, token.cancelled_owned())
            .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, "gnmi spike server stopped with an error");
        }
    });

    Ok(SpikeServer { local_addr, handle })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Instant;

    use tokio_stream::StreamExt;
    use tonic::transport::{Channel, Endpoint};

    /// Open a `Subscribe` stream and return the response stream plus the
    /// sender that keeps the request half open.
    async fn subscribe(
        addr: SocketAddr,
    ) -> (mpsc::Sender<SubscribeRequest>, Streaming<SubscribeResponse>) {
        let channel: Channel = Endpoint::from_shared(format!("http://{addr}"))
            .expect("endpoint")
            .connect()
            .await
            .expect("connect");
        let mut client = tonic::client::Grpc::new(channel);
        client.ready().await.expect("ready");

        let (tx, rx) = mpsc::channel::<SubscribeRequest>(4);
        tx.send(SubscribeRequest {
            subscribe: Some(SubscriptionList {
                subscription: vec![Subscription {
                    path: Some(Path {
                        elem: vec![PathElem {
                            name: "x".to_string(),
                        }],
                    }),
                }],
            }),
        })
        .await
        .expect("send first request");

        let response = client
            .streaming(
                tonic::Request::new(ReceiverStream::new(rx)),
                SUBSCRIBE_PATH.parse().expect("path"),
                SpikeCodec::<SubscribeRequest, SubscribeResponse>::default(),
            )
            .await
            .expect("open subscribe stream");

        (tx, response.into_inner())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spike_bidi_stream_round_trip() {
        let token = CancellationToken::new();
        let server = serve("127.0.0.1:0".parse().expect("addr"), token.clone())
            .await
            .expect("serve");
        let addr = server.local_addr();

        let (tx, mut stream) = subscribe(addr).await;

        // Collect the first message, the sync marker, and everything that
        // arrives within the sampling window.
        let mut messages = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && messages.len() < 4 {
            match tokio::time::timeout_at(deadline.into(), stream.next()).await {
                Ok(Some(Ok(msg))) => messages.push(msg),
                Ok(Some(Err(e))) => panic!("stream error: {e}"),
                Ok(None) => panic!("stream ended early after {} messages", messages.len()),
                Err(_) => break,
            }
        }

        // Count first: a stream that yielded nothing must fail here, not in a
        // pattern match that never runs.
        assert!(
            messages.len() >= 4,
            "expected at least 4 messages (notification, sync, 2 samples), got {}",
            messages.len()
        );

        assert!(
            matches!(
                messages[0].response,
                Some(subscribe_response::Response::Update(_))
            ),
            "first message was not a notification: {:?}",
            messages[0].response
        );
        assert!(
            matches!(
                messages[1].response,
                Some(subscribe_response::Response::SyncResponse(true))
            ),
            "second message was not sync_response=true: {:?}",
            messages[1].response
        );
        for (i, msg) in messages[2..].iter().enumerate() {
            assert!(
                matches!(msg.response, Some(subscribe_response::Response::Update(_))),
                "message {} after sync was not a notification: {:?}",
                i + 2,
                msg.response
            );
        }

        // Close the client half first: graceful shutdown waits for in-flight
        // connections, and this stream would otherwise never end.
        drop(stream);
        drop(tx);
        token.cancel();
        tokio::time::timeout(Duration::from_secs(5), server.wait())
            .await
            .expect("server did not shut down within 5s");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spike_shutdown_closes_port() {
        let token = CancellationToken::new();
        let server = serve("127.0.0.1:0".parse().expect("addr"), token.clone())
            .await
            .expect("serve");
        let addr = server.local_addr();

        // A connection must succeed first, or "connect fails" below would pass
        // against a server that never came up.
        let (tx, stream) = subscribe(addr).await;
        drop(stream);
        drop(tx);

        token.cancel();
        tokio::time::timeout(Duration::from_secs(5), server.wait())
            .await
            .expect("server did not shut down within 5s");

        let refused = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match tokio::net::TcpStream::connect(addr).await {
                    Err(_) => return true,
                    Ok(conn) => {
                        drop(conn);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        })
        .await;

        assert_eq!(
            refused,
            Ok(true),
            "port {addr} still accepted connections 1s after cancellation"
        );
    }
}
