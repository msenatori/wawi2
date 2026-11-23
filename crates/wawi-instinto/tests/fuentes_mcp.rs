//! The `mcp` source against a local streamable-HTTP MCP server, installed
//! as a plugin in a temporary home: it reads with a read tool, refuses act
//! tools (by plugin.toml and by the server's hint), resolves secrets and
//! keeps them out of errors.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use wawi_instinto::clock::SystemClock;
use wawi_instinto::fuentes::{Ctx, Registro};
use wawi_instinto::spec::Fuente;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("instinto-mcp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let len = text[..end]
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length: ")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

/// A fake GitHub-ish MCP server: `get_pull_request` (read-only hint),
/// `merge` (no hint, so act), `listed_act` (read hint, but plugin.toml
/// says act). It checks the bearer token.
fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let request = read_request(&mut stream);
            let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
            let message: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let authorized = request
                .to_lowercase()
                .contains("authorization: bearer tok-123");
            let reply = match (message["method"].as_str(), message.get("id")) {
                (_, None) => None,
                _ if !authorized => {
                    let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
                (Some("initialize"), Some(id)) => Some(
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": "2025-06-18", "serverInfo": { "name": "gh" } } }),
                ),
                (Some("tools/list"), Some(id)) => {
                    Some(json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": [
                    { "name": "get_pull_request", "inputSchema": { "type": "object" }, "annotations": { "readOnlyHint": true } },
                    { "name": "merge", "inputSchema": { "type": "object" } },
                    { "name": "listed_act", "inputSchema": { "type": "object" }, "annotations": { "readOnlyHint": true } }
                ] } }))
                }
                (Some("tools/call"), Some(id)) => {
                    let n = message["params"]["arguments"]["number"]
                        .as_u64()
                        .unwrap_or(0);
                    let text = if n == 7 {
                        json!({ "number": 7, "reviewDecision": "APPROVED" }).to_string()
                    } else {
                        "no existe ese PR".into()
                    };
                    Some(
                        json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": text }], "isError": n != 7 } }),
                    )
                }
                (_, Some(id)) => Some(
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "no" } }),
                ),
            };
            let response = match reply {
                Some(reply) => {
                    let body = reply.to_string();
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
                None => {
                    "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
                }
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    url
}

fn install(home: &std::path::Path, url: &str, secret: &str) {
    let dir = home.join("plugins/github");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("timeout = \"5s\"\n[server]\nurl = \"{url}\"\nheaders = {{ Authorization = \"Bearer ${{GH_TOKEN}}\" }}\n[tools.listed_act]\nkind = \"act\"\n"),
    )
    .unwrap();
    let secrets = home.join("secrets.toml");
    std::fs::write(&secrets, format!("GH_TOKEN = \"{secret}\"\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn fuente(tool: &str, number: u64) -> Fuente {
    Fuente {
        tipo: "mcp".into(),
        config: json!({ "plugin": "github", "tool": tool, "args": { "number": number } }),
    }
}

#[test]
fn reads_with_a_read_tool_and_refuses_act_tools() {
    let home = temp("read");
    let url = serve();
    install(&home, &url, "tok-123");
    let clock = SystemClock::new();
    let ctx = Ctx {
        home: &home,
        clock: &clock,
        timeout: Duration::from_secs(5),
    };
    let r = Registro::base();
    assert!(r.validar(&fuente("get_pull_request", 7), &ctx).is_ok());
    assert_eq!(
        r.leer(&fuente("get_pull_request", 7), &ctx).unwrap()["reviewDecision"],
        "APPROVED"
    );
    let error = r.leer(&fuente("get_pull_request", 8), &ctx).unwrap_err();
    assert!(
        error.contains("dio error") && error.contains("no existe"),
        "{error}"
    );
    // plugin.toml says act: refused without connecting.
    assert!(
        r.validar(&fuente("listed_act", 7), &ctx)
            .unwrap_err()
            .contains("kind = act")
    );
    // No hint at all: act by default, refused at call time.
    assert!(r.validar(&fuente("merge", 7), &ctx).is_ok());
    assert!(
        r.leer(&fuente("merge", 7), &ctx)
            .unwrap_err()
            .contains("no es de lectura")
    );
    // Unknown plugin, and itself.
    let other = Fuente {
        tipo: "mcp".into(),
        config: json!({ "plugin": "linear", "tool": "x" }),
    };
    assert!(
        r.validar(&other, &ctx)
            .unwrap_err()
            .contains("no hay un plugin")
    );
    let me = Fuente {
        tipo: "mcp".into(),
        config: json!({ "plugin": "instinto", "tool": "x" }),
    };
    assert!(r.validar(&me, &ctx).unwrap_err().contains("sí mismo"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_rejected_secret_never_shows_in_the_error() {
    let home = temp("secret");
    let url = serve();
    install(&home, &url, "wrong-secret-value");
    let clock = SystemClock::new();
    let ctx = Ctx {
        home: &home,
        clock: &clock,
        timeout: Duration::from_secs(5),
    };
    let error = Registro::base()
        .leer(&fuente("get_pull_request", 7), &ctx)
        .unwrap_err();
    assert!(error.contains("credencial"), "{error}");
    assert!(!error.contains("wrong-secret-value"));
    let _ = std::fs::remove_dir_all(&home);
}
