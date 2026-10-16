//! The gRPC service a [`GnmiTarget`] serves: a hand-written router over
//! `http::Request` in place of generated server code.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Weak;
use std::task::{Context, Poll};

use tonic::server::NamedService;
use tonic::Status;
use tower::Service;

use super::target::GnmiTarget;

/// The gRPC service name of gNMI.
pub const SERVICE_NAME: &str = "gnmi.gNMI";

/// A boxed future returning `Result<T, E>`.
type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;

/// Routes HTTP/2 requests by path for one target.
///
/// Every path, `/gnmi.gNMI/Subscribe` included, answers `UNIMPLEMENTED`. The
/// service holds its target weakly, so the listener never keeps the target
/// alive.
#[derive(Clone)]
pub struct GnmiService {
    #[allow(dead_code)]
    target: Weak<GnmiTarget>,
}

impl GnmiService {
    /// A service for `target`.
    pub(crate) fn new(target: Weak<GnmiTarget>) -> Self {
        Self { target }
    }
}

/// Matches the service name; nothing routes on it.
impl NamedService for GnmiService {
    const NAME: &'static str = SERVICE_NAME;
}

impl Service<http::Request<tonic::body::Body>> for GnmiService {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<tonic::body::Body>) -> Self::Future {
        let status = Status::unimplemented(format!(
            "{}: gnmi service arrives in a later change",
            req.uri().path()
        ));
        Box::pin(async move { Ok(status.into_http()) })
    }
}
