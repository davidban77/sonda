//! Certificates for the TLS tests: a CA, and a server and a client
//! certificate it signed, made with `rcgen` and written as PEM files into a
//! temporary directory.

use std::net::SocketAddr;
use std::path::PathBuf;

use rcgen::{BasicConstraints, Certificate, CertificateParams, IsCa, KeyPair};
use tempfile::TempDir;
use tokio_stream::StreamExt;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint, Identity};

use super::GnmiTlsConfig;
use crate::encoder::gnmi::proto::{self, subscribe_request, subscribe_response, subscription_list};
use crate::sink::grpc_codec::ProstCodec;

/// The server certificate's names: `localhost` and `127.0.0.1`.
pub(crate) const SERVER_NAMES: [&str; 2] = ["localhost", "127.0.0.1"];

/// A CA, a server certificate for [`SERVER_NAMES`] and a client
/// certificate, both signed by the CA, as PEM files in a directory removed
/// when this drops: `ca.pem`, `server.pem`, `server-key.pem`, `client.pem`
/// and `client-key.pem`.
pub(crate) struct TlsFiles {
    dir: TempDir,
    /// The CA certificate.
    pub(crate) ca_pem: String,
    /// The client certificate and its key.
    pub(crate) client_pem: String,
    pub(crate) client_key_pem: String,
}

impl TlsFiles {
    pub(crate) fn new() -> Self {
        let ca_key = KeyPair::generate().expect("a CA key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).expect("a CA certificate");
        let (server_pem, server_key_pem) = signed(
            SERVER_NAMES.iter().map(|s| s.to_string()).collect(),
            &ca,
            &ca_key,
        );
        let (client_pem, client_key_pem) = signed(vec!["client".to_string()], &ca, &ca_key);
        let files = Self {
            dir: tempfile::tempdir().expect("a temporary directory"),
            ca_pem: ca.pem(),
            client_pem,
            client_key_pem,
        };
        files.write("ca.pem", &files.ca_pem);
        files.write("server.pem", &server_pem);
        files.write("server-key.pem", &server_key_pem);
        files.write("client.pem", &files.client_pem);
        files.write("client-key.pem", &files.client_key_pem);
        files
    }

    /// The path of `name` in the directory, as text.
    pub(crate) fn path(&self, name: &str) -> String {
        self.dir.path().join(name).display().to_string()
    }

    /// Write `contents` to `name` in the directory and return its path.
    pub(crate) fn write(&self, name: &str, contents: &str) -> String {
        let path: PathBuf = self.dir.path().join(name);
        std::fs::write(&path, contents).expect("write a test file");
        path.display().to_string()
    }

    /// A `tls:` block serving the server certificate, any client allowed.
    pub(crate) fn tls(&self) -> GnmiTlsConfig {
        GnmiTlsConfig {
            cert: self.path("server.pem"),
            key: self.path("server-key.pem"),
            client_ca: None,
        }
    }

    /// [`tls`](Self::tls) requiring a client certificate signed by the CA.
    pub(crate) fn mutual_tls(&self) -> GnmiTlsConfig {
        GnmiTlsConfig {
            client_ca: Some(self.path("ca.pem")),
            ..self.tls()
        }
    }

    /// A channel to `addr` over TLS, trusting the CA and verifying the
    /// server as `localhost`, presenting the client certificate when
    /// `present_client_cert`.
    pub(crate) async fn channel(
        &self,
        addr: SocketAddr,
        present_client_cert: bool,
    ) -> Result<Channel, tonic::transport::Error> {
        let mut tls = ClientTlsConfig::new()
            .ca_certificate(tonic::transport::Certificate::from_pem(&self.ca_pem))
            .domain_name("localhost");
        if present_client_cert {
            tls = tls.identity(Identity::from_pem(&self.client_pem, &self.client_key_pem));
        }
        Endpoint::from_shared(format!("https://{addr}"))
            .expect("a valid uri")
            .tls_config(tls)?
            .connect()
            .await
    }
}

/// A certificate for `names` signed by `ca`, and its key, as PEM.
fn signed(names: Vec<String>, ca: &Certificate, ca_key: &KeyPair) -> (String, String) {
    let key = KeyPair::generate().expect("a key");
    let cert = CertificateParams::new(names)
        .expect("certificate params")
        .signed_by(&key, ca, ca_key)
        .expect("a signed certificate");
    (cert.pem(), key.serialize_pem())
}

/// Subscribe ONCE to every path on `channel`, and read the responses to the
/// end: whether the stream carried the sync marker.
pub(crate) async fn once_syncs(channel: Channel) -> Result<bool, tonic::Status> {
    let list = proto::SubscriptionList {
        subscription: vec![proto::Subscription {
            path: Some(proto::Path::default()),
            ..Default::default()
        }],
        mode: subscription_list::Mode::Once as i32,
        ..Default::default()
    };
    let request = proto::SubscribeRequest {
        request: Some(subscribe_request::Request::Subscribe(list)),
    };
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready()
        .await
        .map_err(|e| tonic::Status::unavailable(e.to_string()))?;
    let mut responses = grpc
        .streaming(
            tonic::Request::new(tokio_stream::iter([request])),
            http::uri::PathAndQuery::from_static("/gnmi.gNMI/Subscribe"),
            ProstCodec::<proto::SubscribeRequest, proto::SubscribeResponse>::default(),
        )
        .await?
        .into_inner();
    let mut synced = false;
    while let Some(response) = responses.next().await {
        if let Some(subscribe_response::Response::SyncResponse(true)) = response?.response {
            synced = true;
        }
    }
    Ok(synced)
}
