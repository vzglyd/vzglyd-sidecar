use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::sync::{Arc, OnceLock};

use rustls::{ClientConfig, ClientConnection, StreamOwned};
use rustls_pki_types::ServerName;

use crate::Error;
use crate::socket::RawSocket;

static TLS_CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();

pub(crate) struct TlsStream {
    inner: StreamOwned<ClientConnection, RawSocket>,
}

pub(crate) fn tls_connect(host: &str, port: u16, ip: Ipv4Addr) -> Result<TlsStream, Error> {
    let socket = RawSocket::connect(ip, port)?;
    let server_name = ServerName::try_from(host)
        .map_err(|error| Error::Tls(format!("invalid TLS server name '{host}': {error}")))?
        .to_owned();
    let connection = ClientConnection::new(shared_tls_config(), server_name)
        .map_err(|error| Error::Tls(format!("failed to create TLS session: {error}")))?;
    Ok(TlsStream {
        inner: StreamOwned::new(connection, socket),
    })
}

fn shared_tls_config() -> Arc<ClientConfig> {
    TLS_CONFIG.get_or_init(build_tls_config).clone()
}

fn build_tls_config() -> Arc<ClientConfig> {
    let root_store = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let tls_config = ClientConfig::builder_with_provider(rustls_rustcrypto::provider().into())
        .with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
        .expect("vzglyd_sidecar protocol set is supported")
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Arc::new(tls_config)
}

impl Read for TlsStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for TlsStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
