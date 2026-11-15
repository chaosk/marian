//! Connecting to Postgres, with TLS as `sslmode` in `DATABASE_URL` asks.
//!
//! libpq read `sslmode` itself; tokio-postgres only knows `disable`, `prefer`
//! and `require`, and verifies nothing on its own. So Marian takes the TLS
//! settings out of the URL and makes the connection the way libpq would:
//!
//! - `disable`: no TLS.
//! - `allow`, `prefer` (the default): TLS when the server offers it, plain
//!   when it doesn't. The certificate isn't checked, as with libpq when no
//!   root certificate is configured. (libpq's `allow` tries plain first;
//!   here it tries TLS first, like `prefer`.)
//! - `require`: TLS or no connection. The certificate is checked only if
//!   `sslrootcert` names the certificates to trust, as with libpq.
//! - `verify-ca`: TLS, and the server's certificate must chain to a trusted
//!   root.
//! - `verify-full`: as `verify-ca`, and the certificate must name the host.
//!
//! The trusted roots are the PEM file `sslrootcert` names, or the system's
//! (which honour `SSL_CERT_FILE`, as the image sets) when it's left out or
//! says `system`. libpq would look in `~/.postgresql/root.crt` instead.
//! Client certificates (`sslcert`, `sslkey`) aren't supported, and SCRAM
//! channel binding isn't offered, so `channel_binding=require` won't connect.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::config::SslMode as WireMode;
use tokio_postgres::tls::{ChannelBinding, MakeTlsConnect, TlsConnect};

/// What `sslmode` asks of the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SslMode {
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

/// Where to connect, and how.
#[derive(Clone)]
pub struct Target {
    pub config: tokio_postgres::Config,
    pub mode: SslMode,
    tls: Option<Tls>,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target").field("mode", &self.mode).finish()
    }
}

impl Target {
    /// Reads a `postgres://` URL. Fails on TLS settings it can't honour, rather
    /// than connecting with less than the URL asks for.
    pub fn parse(url: &str) -> Result<Self, String> {
        let (base, query) = match url.split_once('?') {
            Some((base, query)) => (base, query),
            None => (url, ""),
        };
        let mut mode = SslMode::Prefer;
        let mut root_cert = None;
        let mut kept = Vec::new();
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = percent_decode(value);
            match key {
                "sslmode" => {
                    mode = match value.as_str() {
                        "disable" => SslMode::Disable,
                        "allow" | "prefer" => SslMode::Prefer,
                        "require" => SslMode::Require,
                        "verify-ca" => SslMode::VerifyCa,
                        "verify-full" => SslMode::VerifyFull,
                        other => {
                            return Err(format!(
                                "sslmode={other} isn't one of disable, allow, prefer, require, verify-ca or verify-full."
                            ));
                        }
                    }
                }
                "sslrootcert" => root_cert = Some(value),
                "sslcert" | "sslkey" | "sslpassword" | "sslcrl" | "sslcrldir" => {
                    return Err(format!(
                        "{key} isn't supported: Marian doesn't send client certificates or read CRLs."
                    ));
                }
                _ => kept.push(pair),
            }
        }
        let url = if kept.is_empty() {
            base.to_string()
        } else {
            format!("{base}?{}", kept.join("&"))
        };
        let mut config: tokio_postgres::Config = url.parse().map_err(|err| {
            format!("DATABASE_URL isn't a connection string Marian can read: {err}")
        })?;
        config.ssl_mode(match mode {
            SslMode::Disable => WireMode::Disable,
            SslMode::Prefer => WireMode::Prefer,
            SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull => WireMode::Require,
        });
        let tls = match mode {
            SslMode::Disable => None,
            _ => Some(Tls::new(mode, root_cert.as_deref())?),
        };
        Ok(Self { config, mode, tls })
    }

    /// Opens a connection: the client, and the connection task to drive it.
    pub async fn connect(&self) -> Result<diesel_async::AsyncPgConnection, String> {
        use diesel_async::AsyncPgConnection;
        // tokio-postgres says only "db error" or "error connecting to server";
        // what went wrong is in the errors it wraps.
        let failed = |err: &dyn std::error::Error| {
            let mut text = err.to_string();
            let mut source = err.source();
            while let Some(err) = source {
                text = format!("{text}: {err}");
                source = err.source();
            }
            text
        };
        match &self.tls {
            None => {
                let (client, connection) = self
                    .config
                    .connect(tokio_postgres::NoTls)
                    .await
                    .map_err(|err| failed(&err))?;
                AsyncPgConnection::try_from_client_and_connection(client, connection)
                    .await
                    .map_err(|err| failed(&err))
            }
            Some(tls) => {
                let (client, connection) = self
                    .config
                    .connect(tls.clone())
                    .await
                    .map_err(|err| failed(&err))?;
                AsyncPgConnection::try_from_client_and_connection(client, connection)
                    .await
                    .map_err(|err| failed(&err))
            }
        }
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = value
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The TLS side of a connection: a rustls client set up for one `sslmode`.
#[derive(Clone)]
struct Tls {
    config: Arc<ClientConfig>,
    names_matter: bool,
}

impl Tls {
    fn new(mode: SslMode, root_cert: Option<&str>) -> Result<Self, String> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|err| err.to_string())?;
        let explicit_roots = root_cert.filter(|path| *path != "system");
        // As libpq: prefer and require check nothing, unless require is given
        // the certificates to trust.
        let verify = match mode {
            SslMode::Disable | SslMode::Prefer => false,
            SslMode::Require => explicit_roots.is_some(),
            SslMode::VerifyCa | SslMode::VerifyFull => true,
        };
        let config = if verify {
            let roots = Arc::new(roots(explicit_roots)?);
            let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(
                roots,
                provider.clone(),
            )
            .build()
            .map_err(|err| format!("Could not set up TLS for Postgres: {err}"))?;
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(Verifier {
                    webpki: Some(webpki),
                    check_name: mode == SslMode::VerifyFull,
                    provider,
                }))
                .with_no_client_auth()
        } else {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(Verifier {
                    webpki: None,
                    check_name: false,
                    provider,
                }))
                .with_no_client_auth()
        };
        Ok(Self {
            config: Arc::new(config),
            names_matter: mode == SslMode::VerifyFull,
        })
    }
}

fn roots(path: Option<&str>) -> Result<rustls::RootCertStore, String> {
    let mut store = rustls::RootCertStore::empty();
    match path {
        Some(path) => {
            let pem = std::fs::read(path)
                .map_err(|err| format!("Could not read sslrootcert {path}: {err}"))?;
            let mut reader = io::BufReader::new(pem.as_slice());
            let certs: Vec<CertificateDer<'static>> =
                rustls::pki_types::pem::PemObject::pem_reader_iter(&mut reader)
                    .collect::<Result<_, _>>()
                    .map_err(|err| format!("Could not read sslrootcert {path}: {err:?}"))?;
            for cert in certs {
                store.add(cert).map_err(|err| {
                    format!("sslrootcert {path} holds an unusable certificate: {err}")
                })?;
            }
        }
        None => {
            let found = rustls_native_certs::load_native_certs();
            for cert in found.certs {
                let _ = store.add(cert);
            }
        }
    }
    if store.is_empty() {
        return Err(match path {
            Some(path) => format!("sslrootcert {path} holds no certificates."),
            None => "Found no trusted root certificates to check the Postgres server's against; set sslrootcert or SSL_CERT_FILE.".into(),
        });
    }
    Ok(store)
}

/// Checks the server's certificate as far as `sslmode` asks: not at all
/// (`webpki` is `None`), its chain (`verify-ca`), or its chain and name
/// (`verify-full`). The handshake's signatures are always checked, so the
/// server holds the key for the certificate it sent.
#[derive(Debug)]
struct Verifier {
    webpki: Option<Arc<rustls::client::WebPkiServerVerifier>>,
    check_name: bool,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let Some(webpki) = &self.webpki else {
            return Ok(ServerCertVerified::assertion());
        };
        match webpki.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidForName
                | rustls::CertificateError::NotValidForNameContext { .. },
            )) if !self.check_name => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl<S> MakeTlsConnect<S> for Tls
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = TlsStream<S>;
    type TlsConnect = Connect;
    type Error = rustls::pki_types::InvalidDnsNameError;

    fn make_tls_connect(&mut self, domain: &str) -> Result<Connect, Self::Error> {
        let name = match ServerName::try_from(domain.to_string()) {
            Ok(name) => name,
            // Only verify-full reads the name; the others still need one to
            // start the handshake.
            Err(_) if !self.names_matter => ServerName::try_from("postgres".to_string())?,
            Err(err) => return Err(err),
        };
        Ok(Connect {
            connector: tokio_rustls::TlsConnector::from(self.config.clone()),
            name,
        })
    }
}

pub struct Connect {
    connector: tokio_rustls::TlsConnector,
    name: ServerName<'static>,
}

impl<S> TlsConnect<S> for Connect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = TlsStream<S>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<TlsStream<S>>> + Send>>;

    fn connect(self, stream: S) -> Self::Future {
        Box::pin(async move {
            let stream = self.connector.connect(self.name, stream).await?;
            Ok(TlsStream(Box::new(stream)))
        })
    }
}

pub struct TlsStream<S>(Box<tokio_rustls::client::TlsStream<S>>);

impl<S> tokio_postgres::tls::TlsStream for TlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn channel_binding(&self) -> ChannelBinding {
        ChannelBinding::none()
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for TlsStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(cx, buf)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for TlsStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sslmode_picks_what_the_connection_asks_of_tls() {
        let mode = |url: &str| Target::parse(url).map(|target| target.mode);
        let base = "postgres://marian:marian@127.0.0.1:5432/marian";
        assert_eq!(mode(base), Ok(SslMode::Prefer));
        assert_eq!(
            mode(&format!("{base}?sslmode=disable")),
            Ok(SslMode::Disable)
        );
        assert_eq!(mode(&format!("{base}?sslmode=allow")), Ok(SslMode::Prefer));
        assert_eq!(
            mode(&format!("{base}?sslmode=require")),
            Ok(SslMode::Require)
        );
        assert_eq!(
            mode(&format!("{base}?application_name=x&sslmode=verify-full")),
            Ok(SslMode::VerifyFull)
        );
        assert!(mode(&format!("{base}?sslmode=sometimes")).is_err());
        assert!(mode(&format!("{base}?sslmode=require&sslcert=/tmp/me.crt")).is_err());
        assert!(
            mode(&format!(
                "{base}?sslmode=verify-ca&sslrootcert=/no/such/file"
            ))
            .is_err()
        );
    }

    #[test]
    fn the_other_settings_stay_in_the_url() {
        let target = Target::parse(
            "postgres://marian@db:6432/marian?sslmode=require&application_name=web&connect_timeout=3",
        )
        .unwrap();
        assert_eq!(target.config.get_application_name(), Some("web"));
        assert_eq!(
            target.config.get_connect_timeout(),
            Some(&std::time::Duration::from_secs(3))
        );
        assert_eq!(target.config.get_ssl_mode(), WireMode::Require);
    }

    #[test]
    fn a_root_certificate_path_is_unescaped() {
        assert_eq!(percent_decode("%2Fetc%2Fssl%2Fca.pem"), "/etc/ssl/ca.pem");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("50%"), "50%");
    }
}
