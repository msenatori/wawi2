//! `{"http": {"url": "https://...", "headers"?: {...}, "timeout"?: "10s"}}`:
//! one GET. A timeout, a size cap, and redirects followed by hand so none
//! lands on a private, loopback or link-local address. Header values are
//! literal: `${SECRET}` is refused, so a spec (which the brain may write)
//! can never carry a key to some URL.

use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use serde_json::Value;

use super::{Ctx, TipoFuente};
use crate::extraer::as_json;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;

#[derive(Clone, Debug)]
pub struct Http {
    /// The largest body read.
    pub max_bytes: u64,
}

impl Default for Http {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug)]
struct Spec {
    url: String,
    headers: Vec<(String, String)>,
    timeout: Duration,
}

fn spec(config: &Value) -> Result<Spec, String> {
    let url = config
        .get("url")
        .and_then(Value::as_str)
        .ok_or("http necesita url")?
        .trim()
        .to_owned();
    Url::parse(&url)?;
    let mut headers = Vec::new();
    if let Some(map) = config.get("headers") {
        let map = map
            .as_object()
            .ok_or("http.headers es una tabla de textos")?;
        for (k, v) in map {
            let v = v.as_str().ok_or("http.headers es una tabla de textos")?;
            if v.contains("${") {
                return Err(
                    "http.headers no lleva claves (${…}): para una API con clave, usá un plugin"
                        .into(),
                );
            }
            headers.push((k.clone(), v.to_owned()));
        }
    }
    let timeout = match config.get("timeout").and_then(Value::as_str) {
        Some(text) => wawi_plugins::schedule::parse_duration(text)?.min(MAX_TIMEOUT),
        None => DEFAULT_TIMEOUT,
    };
    for key in config.as_object().into_iter().flat_map(|m| m.keys()) {
        if !["url", "headers", "timeout"].contains(&key.as_str()) {
            return Err(format!(
                "http no conoce «{key}» (solo GET: url, headers, timeout)"
            ));
        }
    }
    Ok(Spec {
        url,
        headers,
        timeout,
    })
}

impl TipoFuente for Http {
    fn validar(&self, config: &Value, _ctx: &Ctx<'_>) -> Result<(), String> {
        spec(config).map(|_| ())
    }

    fn leer(&self, config: &Value, ctx: &Ctx<'_>) -> Result<Value, String> {
        let spec = spec(config)?;
        let timeout = spec.timeout.min(ctx.timeout);
        self.get(&spec.url, &spec.headers, timeout)
            .map(|text| as_json(&text))
    }
}

impl Http {
    /// GET `url`, following up to five redirects, none to a private
    /// address. The body as text.
    pub fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
        timeout: Duration,
    ) -> Result<String, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(timeout))
            .user_agent("wawi-instinto")
            .build()
            .into();
        let mut url = Url::parse(url)?;
        for _ in 0..=MAX_REDIRECTS {
            let mut request = agent.get(&url.text);
            for (k, v) in headers {
                request = request.header(k.as_str(), v.as_str());
            }
            let mut response = request.call().map_err(|e| match e {
                ureq::Error::Timeout(_) => "tardó demasiado".to_owned(),
                other => format!("no pude leer {}: {other}", url.host),
            })?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| format!("HTTP {status} sin destino"))?;
                let next = url.join(location)?;
                if host_is_private(&next.host, next.port)? {
                    return Err(format!(
                        "una redirección quiso ir a una dirección privada ({}); no la sigo",
                        next.host
                    ));
                }
                url = next;
                continue;
            }
            if !(200..300).contains(&status) {
                return Err(format!("HTTP {status}"));
            }
            return response
                .body_mut()
                .with_config()
                .limit(self.max_bytes)
                .read_to_string()
                .map_err(|e| match e {
                    ureq::Error::Timeout(_) => "tardó demasiado".to_owned(),
                    ureq::Error::BodyExceedsLimit(_) => {
                        format!("la respuesta pasa de {} KB", self.max_bytes / 1024)
                    }
                    other => format!("no pude leer la respuesta: {other}"),
                });
        }
        Err(format!("más de {MAX_REDIRECTS} redirecciones"))
    }
}

/// Just enough URL to follow a redirect.
#[derive(Clone, Debug, PartialEq)]
struct Url {
    text: String,
    scheme: String,
    host: String,
    port: u16,
    /// The path and query, starting with `/`.
    path: String,
}

impl Url {
    fn parse(text: &str) -> Result<Self, String> {
        let (scheme, rest) = text
            .split_once("://")
            .ok_or_else(|| format!("url «{text}» tiene que empezar con https:// o http://"))?;
        let scheme = scheme.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "https" => 443,
            "http" => 80,
            _ => return Err(format!("url «{text}»: solo http y https")),
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        if authority.contains('@') {
            return Err("url con usuario y clave: poné la clave en headers con ${SECRETO}".into());
        }
        let path = match &rest[end..] {
            "" => "/".to_owned(),
            p if p.starts_with('/') => p.to_owned(),
            p => format!("/{p}"),
        };
        let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
            let close = inner.find(']').ok_or("url con IPv6 sin cerrar")?;
            let host = &inner[..close];
            let port = inner[close + 1..]
                .strip_prefix(':')
                .map(|p| p.parse::<u16>().map_err(|_| "puerto inválido".to_owned()))
                .transpose()?;
            (host.to_owned(), port.unwrap_or(default_port))
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (
                    h.to_owned(),
                    p.parse::<u16>().map_err(|_| "puerto inválido".to_owned())?,
                ),
                None => (authority.to_owned(), default_port),
            }
        };
        if host.is_empty() {
            return Err(format!("url «{text}» sin servidor"));
        }
        Ok(Self {
            text: text.to_owned(),
            scheme,
            host: host.to_ascii_lowercase(),
            port,
            path,
        })
    }

    fn join(&self, location: &str) -> Result<Self, String> {
        let location = location.trim();
        if location.contains("://") {
            return Url::parse(location);
        }
        let origin = if self.host.contains(':') {
            format!("{}://[{}]:{}", self.scheme, self.host, self.port)
        } else {
            format!("{}://{}:{}", self.scheme, self.host, self.port)
        };
        if let Some(rest) = location.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", self.scheme));
        }
        if location.starts_with('/') {
            return Url::parse(&format!("{origin}{location}"));
        }
        let path = self.path.split(['?', '#']).next().unwrap_or("/");
        let dir = &path[..path.rfind('/').map_or(1, |i| i + 1)];
        Url::parse(&format!("{origin}{dir}{location}"))
    }
}

/// Whether `host` is, or resolves to, an address that is not public.
fn host_is_private(host: &str, port: u16) -> Result<bool, String> {
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return Ok(true);
    }
    let ips: Vec<IpAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => (host, port)
            .to_socket_addrs()
            .map_err(|_| format!("no encontré {host}"))?
            .map(|a| a.ip())
            .collect(),
    };
    Ok(ips.is_empty() || ips.iter().any(|ip| is_private(*ip)))
}

pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// A local server that answers each connection with the next response.
    fn serve(responses: Vec<String>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                seen.push(String::from_utf8_lossy(&buf[..n]).to_string());
                let _ = stream.write_all(response.as_bytes());
            }
            seen
        });
        (base, handle)
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn reads_json_and_sends_headers() {
        let (base, server) = serve(vec![ok(r#"{"venta": 980}"#)]);
        let http = Http::default();
        let body = http
            .get(
                &format!("{base}/v1/dolar"),
                &[("X-Clave".into(), "abc".into())],
                DEFAULT_TIMEOUT,
            )
            .unwrap();
        assert_eq!(as_json(&body)["venta"], 980);
        let seen = server.join().unwrap();
        assert!(seen[0].starts_with("GET /v1/dolar"));
        assert!(seen[0].to_lowercase().contains("x-clave: abc"));
    }

    #[test]
    fn refuses_a_redirect_to_a_private_address() {
        let redirect = "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/secreto\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
        let (base, server) = serve(vec![redirect]);
        let error = Http::default()
            .get(&format!("{base}/a"), &[], DEFAULT_TIMEOUT)
            .unwrap_err();
        assert!(error.contains("privada"), "{error}");
        server.join().unwrap();
    }

    #[test]
    fn caps_the_size_and_reports_status() {
        let big = "x".repeat(5000);
        let (base, server) = serve(vec![
            ok(&big),
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        ]);
        let http = Http { max_bytes: 1000 };
        assert!(
            http.get(&format!("{base}/"), &[], DEFAULT_TIMEOUT)
                .unwrap_err()
                .contains("KB")
        );
        assert_eq!(
            http.get(&format!("{base}/"), &[], DEFAULT_TIMEOUT)
                .unwrap_err(),
            "HTTP 404"
        );
        server.join().unwrap();
    }

    #[test]
    fn times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/lento", listener.local_addr().unwrap());
        let hold = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(1500));
            drop(stream);
        });
        let error = Http::default()
            .get(&url, &[], Duration::from_millis(300))
            .unwrap_err();
        assert!(error.contains("tardó"), "{error}");
        hold.join().unwrap();
    }

    #[test]
    fn urls_and_addresses() {
        let u = Url::parse("https://Api.Example.com/v1/x?a=1").unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.path.as_str()),
            ("api.example.com", 443, "/v1/x?a=1")
        );
        assert_eq!(u.join("/y").unwrap().text, "https://api.example.com:443/y");
        assert_eq!(
            u.join("z").unwrap().text,
            "https://api.example.com:443/v1/z"
        );
        assert_eq!(u.join("http://otro.com/").unwrap().host, "otro.com");
        assert!(Url::parse("ftp://x").is_err());
        assert!(Url::parse("https://user:pw@x.com").is_err());
        for private in [
            "10.0.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_private(private.parse().unwrap()), "{private}");
        }
        for public in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
            assert!(!is_private(public.parse().unwrap()), "{public}");
        }
        assert!(host_is_private("localhost", 80).unwrap());
        let config = serde_json::json!({ "url": "https://x.com", "method": "POST" });
        assert!(spec(&config).unwrap_err().contains("solo GET"));
        let config = serde_json::json!({ "url": "https://x.com", "headers": { "Authorization": "Bearer ${LINEAR_API_KEY}" } });
        assert!(spec(&config).unwrap_err().contains("claves"));
    }
}
