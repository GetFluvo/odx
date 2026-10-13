use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;

use crate::ForgeError;

/// A response: status, headers (names lowercase) and body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers, names lowercase.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// The first value of header `name` (lowercase).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// HTTP, so forge clients can be tested without a network.
pub trait Http: fmt::Debug + Send + Sync {
    /// GET `url` with `headers`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Http`] when no response arrives (DNS, TLS, connection).
    fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<HttpResponse, ForgeError>;

    /// POST `body` as JSON to `url` with `headers`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Http`] when no response arrives (DNS, TLS, connection).
    fn post(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &serde_json::Value,
    ) -> Result<HttpResponse, ForgeError>;
}

/// [`Http`] over `reqwest` with rustls, verifying certificates against the
/// operating system's trust store.
#[derive(Debug)]
pub struct ReqwestHttp {
    client: reqwest::blocking::Client,
}

impl ReqwestHttp {
    /// A client with a 30 second timeout and the `odx/<version>` user agent.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Http`] when the TLS configuration cannot be built.
    pub fn new() -> Result<Self, ForgeError> {
        static PROVIDER: OnceLock<()> = OnceLock::new();
        PROVIDER.get_or_init(|| {
            // Fails only when another provider was installed first, which
            // serves just as well.
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("odx/", env!("CARGO_PKG_VERSION")))
            .build()
            .map(|client| Self { client })
            .map_err(ForgeError::from)
    }
}

impl From<reqwest::Error> for ForgeError {
    /// The error without its URL, which may carry credentials.
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error.without_url().to_string())
    }
}

impl ReqwestHttp {
    fn execute(
        request: reqwest::blocking::RequestBuilder,
        headers: &[(&str, String)],
    ) -> Result<HttpResponse, ForgeError> {
        let request = headers.iter().fold(request, |request, (name, value)| {
            request.header(*name, value)
        });
        let response = request.send().map_err(ForgeError::from)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = response.bytes().map_err(ForgeError::from)?.to_vec();
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

impl Http for ReqwestHttp {
    fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<HttpResponse, ForgeError> {
        Self::execute(self.client.get(url), headers)
    }

    fn post(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &serde_json::Value,
    ) -> Result<HttpResponse, ForgeError> {
        Self::execute(self.client.post(url).json(body), headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serve `response` to one connection; returns the URL and the request.
    fn serve_once(response: &'static [u8]) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/repos/a/b", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0_u8; 4096];
            let n = stream.read(&mut request).unwrap();
            stream.write_all(response).unwrap();
            String::from_utf8_lossy(&request[..n]).into_owned()
        });
        (url, handle)
    }

    #[test]
    fn gets_status_headers_and_body() {
        let (url, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: \"x\"\r\nContent-Length: 2\r\n\r\n{}",
        );
        let http = ReqwestHttp::new().unwrap();
        let response = http
            .get(&url, &[("authorization", "Bearer secret".to_owned())])
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.header("etag"), Some("\"x\""));
        assert_eq!(response.header("missing"), None);
        assert_eq!(response.body, b"{}");
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(
            request.contains("authorization: bearer secret"),
            "{request}"
        );
        assert!(request.contains("user-agent: odx/"), "{request}");
    }

    #[test]
    fn connection_and_body_failures() {
        let http = ReqwestHttp::new().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let err = http.get(&closed, &[]).unwrap_err();
        assert!(err.to_string().starts_with("request failed: "));
        assert!(
            !err.to_string().contains(&closed),
            "URLs are not repeated: {err}"
        );

        // The body is cut short.
        let (url, server) = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial");
        let err = http.get(&url, &[]).unwrap_err();
        assert!(err.to_string().starts_with("request failed: "));
        server.join().unwrap();
    }
}
