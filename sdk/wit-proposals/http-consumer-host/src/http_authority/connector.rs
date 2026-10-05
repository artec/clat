//! Private author transport candidate. No public network entry or HTTP sender.
use super::PreparedRequest;
use crate::dns_authority::{ConnectionPins, Failure, Resolution, Scope};
use rustls::pki_types::ServerName;
use std::{net::SocketAddr, sync::Arc};
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};

pub(super) struct ConnectionRequest {
    pub(super) request: PreparedRequest,
    pub(super) pins: ConnectionPins,
}
pub(super) enum Connection {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}
#[derive(Debug, PartialEq)]
pub(super) enum ConnectFailure {
    Authority(Failure),
    Transport,
    Tls,
}
impl From<Failure> for ConnectFailure {
    fn from(value: Failure) -> Self {
        Self::Authority(value)
    }
}
impl ConnectionRequest {
    pub(super) fn bind(
        request: PreparedRequest,
        resolution: &Resolution,
        scope: &Scope,
    ) -> Result<Self, Failure> {
        let pins = resolution.consume(scope, request.origin())?;
        Ok(Self { request, pins })
    }
    pub(super) async fn connect(&self) -> Result<Connection, ConnectFailure> {
        self.connect_with(TlsConnector::from(Arc::new(default_tls())))
            .await
    }
    async fn connect_with(&self, tls: TlsConnector) -> Result<Connection, ConnectFailure> {
        let (origin, addresses, _) = self.pins.snapshot()?;
        #[cfg(any(test, feature = "test-support"))]
        let addresses = self.pins.fixture_addresses(addresses);
        let operation = async {
            let stream = dial(
                addresses
                    .into_iter()
                    .map(|ip| SocketAddr::new(ip, origin.port())),
            )
            .await?;
            if origin.scheme() == "http" {
                return Ok::<_, ConnectFailure>(Connection::Plain(stream));
            }
            let name =
                ServerName::try_from(origin.host().to_owned()).map_err(|_| ConnectFailure::Tls)?;
            let stream = tls
                .connect(name, stream)
                .await
                .map_err(|_| ConnectFailure::Tls)?;
            Ok(Connection::Tls(Box::new(stream)))
        };
        guarded(&self.pins, operation).await
    }
}
async fn guarded<T>(
    pins: &ConnectionPins,
    operation: impl std::future::Future<Output = Result<T, ConnectFailure>>,
) -> Result<T, ConnectFailure> {
    let connection = tokio::select! {
        biased;
        error = pins.closed() => return Err(error.into()),
        result = operation => result?,
    };
    // A completion racing owner invalidation must never publish a live socket.
    pins.snapshot()?;
    Ok(connection)
}

async fn dial(addresses: impl Iterator<Item = SocketAddr>) -> Result<TcpStream, ConnectFailure> {
    for address in addresses {
        if let Ok(stream) = TcpStream::connect(address).await {
            return Ok(stream);
        }
    }
    Err(ConnectFailure::Transport)
}
fn default_tls() -> rustls::ClientConfig {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}
#[cfg(test)]
mod tests;
