use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

use serde_json::{Value, json};

pub(crate) struct HttpRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query: String,
}

impl HttpRequest {
    pub(crate) fn read(stream: &TcpStream) -> jikji_core::Result<Self> {
        let mut reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|source| jikji_core::io_error("<gui-clone>", source))?,
        );
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .map_err(|source| jikji_core::io_error("<gui-read>", source))?;
        discard_headers(&mut reader)?;
        let parts = request_line.split_whitespace().collect::<Vec<_>>();
        if parts.len() < 2 {
            return Ok(Self {
                method: String::new(),
                path: String::new(),
                query: String::new(),
            });
        }
        let (path, query) = split_target(parts[1]);
        Ok(Self {
            method: parts[0].to_owned(),
            path: path.to_owned(),
            query,
        })
    }
}

pub(crate) struct HttpResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl HttpResponse {
    pub(crate) fn json(status: u16, value: Value) -> Self {
        let body = serde_json::to_vec(&value).unwrap_or_else(|_| b"{\"error\":\"json\"}".to_vec());
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body,
        }
    }

    pub(crate) fn html(status: u16, text: &'static str) -> Self {
        Self {
            status,
            content_type: "text/html; charset=utf-8",
            body: text.as_bytes().to_vec(),
        }
    }

    pub(crate) fn binary(status: u16, body: Vec<u8>, content_type: &'static str) -> Self {
        Self {
            status,
            content_type,
            body,
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            _ => "Internal Server Error",
        };
        let mut head = format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            self.status,
            self.content_type,
            self.body.len()
        )
        .into_bytes();
        head.extend_from_slice(&self.body);
        head
    }
}

pub(crate) fn write_response(
    mut stream: TcpStream,
    response: &HttpResponse,
) -> jikji_core::Result<()> {
    stream
        .write_all(&response.to_bytes())
        .map_err(|source| jikji_core::io_error("<gui-write>", source))
}

pub(crate) fn query_value(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == name {
            return Some(percent_decode(value));
        }
    }
    None
}

pub(crate) fn query_bool(query: &str, name: &str) -> bool {
    matches!(
        query_value(query, name).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn discard_headers(reader: &mut BufReader<TcpStream>) -> jikji_core::Result<()> {
    let mut discard = String::new();
    loop {
        discard.clear();
        let read = reader
            .read_line(&mut discard)
            .map_err(|source| jikji_core::io_error("<gui-header>", source))?;
        if read == 0 || discard == "\r\n" || discard == "\n" {
            return Ok(());
        }
    }
}

fn split_target(target: &str) -> (&str, String) {
    if let Some((path, query)) = target.split_once('?') {
        (path, query.to_owned())
    } else {
        (target, String::new())
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'+' => {
                out.push(b' ');
                idx += 1;
            }
            b'%' if idx + 2 < bytes.len() => {
                if let (Some(high), Some(low)) =
                    (hex_value(bytes[idx + 1]), hex_value(bytes[idx + 2]))
                {
                    out.push((high << 4) | low);
                    idx += 3;
                } else {
                    out.push(bytes[idx]);
                    idx += 1;
                }
            }
            byte => {
                out.push(byte);
                idx += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn malformed_request() -> HttpResponse {
    HttpResponse::json(400, json!({"error": "malformed request"}))
}
