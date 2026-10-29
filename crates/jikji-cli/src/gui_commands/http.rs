use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::TcpStream;

use serde_json::{Value, json};

/// Largest request body read (login form). Anything bigger is ignored.
const MAX_BODY_BYTES: usize = 8 * 1024;
/// Largest request line plus header block read before the request is cut off.
const MAX_HEADER_BYTES: usize = 32 * 1024;
/// How long a client may take to send its request.
const REQUEST_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub(crate) struct HttpRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query: String,
    /// Lower-cased header names with their raw values.
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: String,
}

impl HttpRequest {
    pub(crate) fn read(stream: &TcpStream) -> jikji_core::Result<Self> {
        // A client that stalls mid-request must not hold its thread forever.
        stream
            .set_read_timeout(Some(REQUEST_READ_TIMEOUT))
            .map_err(|source| jikji_core::io_error("<gui-timeout>", source))?;
        let clone = stream
            .try_clone()
            .map_err(|source| jikji_core::io_error("<gui-clone>", source))?;
        // Caps the whole request head, so an endless request line or header cannot grow memory.
        let mut reader = BufReader::new(clone.take((MAX_HEADER_BYTES + MAX_BODY_BYTES) as u64));
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .map_err(|source| jikji_core::io_error("<gui-read>", source))?;
        let headers = read_headers(&mut reader)?;
        let parts = request_line.split_whitespace().collect::<Vec<_>>();
        if parts.len() < 2 || !request_line.ends_with('\n') {
            return Ok(Self::empty());
        }
        let length = headers
            .iter()
            .find(|(name, _)| name == "content-length")
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = String::new();
        if length > 0 && length <= MAX_BODY_BYTES {
            let mut bytes = vec![0; length];
            if reader.read_exact(&mut bytes).is_err() {
                return Ok(Self::empty());
            }
            body = String::from_utf8_lossy(&bytes).into_owned();
        }
        let (path, query) = split_target(parts[1]);
        Ok(Self {
            method: parts[0].to_owned(),
            path: path.to_owned(),
            query,
            headers,
            body,
        })
    }

    fn empty() -> Self {
        Self {
            method: String::new(),
            path: String::new(),
            query: String::new(),
            headers: Vec::new(),
            body: String::new(),
        }
    }

    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Value of a cookie sent with the request.
    pub(crate) fn cookie(&self, name: &str) -> Option<&str> {
        self.header("cookie")?.split(';').find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then_some(value)
        })
    }

    /// A form/query parameter, looked up in the query string first and then the body.
    pub(crate) fn param(&self, name: &str) -> Option<String> {
        query_value(&self.query, name).or_else(|| query_value(&self.body, name))
    }

    /// Whether the client reached us over HTTPS through a proxy or tunnel.
    pub(crate) fn is_https(&self) -> bool {
        self.header("x-forwarded-proto")
            .is_some_and(|value| value.eq_ignore_ascii_case("https"))
    }
}

pub(crate) struct HttpResponse {
    status: u16,
    content_type: String,
    body: Body,
    headers: Vec<(&'static str, String)>,
}

enum Body {
    Bytes(Vec<u8>),
    /// The next `len` bytes of an already-positioned file, streamed so large media never sits
    /// in memory.
    File {
        file: File,
        len: u64,
    },
}

impl HttpResponse {
    pub(crate) fn json(status: u16, value: Value) -> Self {
        let body = serde_json::to_vec(&value).unwrap_or_else(|_| b"{\"error\":\"json\"}".to_vec());
        Self::bytes(status, body, "application/json; charset=utf-8".to_owned())
    }

    pub(crate) fn html(status: u16, text: &'static str) -> Self {
        Self::bytes(
            status,
            text.as_bytes().to_vec(),
            "text/html; charset=utf-8".to_owned(),
        )
    }

    pub(crate) fn binary(status: u16, body: Vec<u8>, content_type: &'static str) -> Self {
        Self::binary_with_content_type(status, body, content_type.to_owned())
    }

    pub(crate) fn binary_with_content_type(
        status: u16,
        body: Vec<u8>,
        content_type: String,
    ) -> Self {
        Self::bytes(status, body, content_type)
    }

    pub(crate) fn empty(status: u16) -> Self {
        Self::bytes(status, Vec::new(), "text/plain; charset=utf-8".to_owned())
    }

    fn bytes(status: u16, body: Vec<u8>, content_type: String) -> Self {
        Self {
            status,
            content_type,
            body: Body::Bytes(body),
            headers: Vec::new(),
        }
    }

    /// Serve `file` (of `size` bytes), honouring a single `Range: bytes=a-b` header.
    /// Answers 206 with `Content-Range` for a valid range and 416 for an unsatisfiable one.
    pub(crate) fn file_range(
        mut file: File,
        size: u64,
        content_type: &'static str,
        range: Option<&str>,
    ) -> Self {
        let parsed = range.map(|value| parse_range(value, size));
        let (status, start, len) = match parsed {
            None | Some(RangeRequest::Ignored) => (200, 0, size),
            Some(RangeRequest::Unsatisfiable) => {
                return Self::empty(416).with_header("Content-Range", format!("bytes */{size}"));
            }
            Some(RangeRequest::Bytes(start, end)) => (206, start, end - start + 1),
        };
        if file.seek(SeekFrom::Start(start)).is_err() {
            return Self::json(500, json!({"error": "could not read file"}));
        }
        let response = Self {
            status,
            content_type: content_type.to_owned(),
            body: Body::File { file, len },
            headers: Vec::new(),
        }
        .with_header("Accept-Ranges", "bytes".to_owned());
        if status == 206 {
            response.with_header(
                "Content-Range",
                format!("bytes {start}-{}/{size}", start + len - 1),
            )
        } else {
            response
        }
    }

    pub(crate) fn with_header(mut self, name: &'static str, value: String) -> Self {
        self.headers.push((name, value));
        self
    }

    pub(crate) fn status(&self) -> u16 {
        self.status
    }

    #[cfg(test)]
    pub(crate) fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn head(&self, length: u64) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            202 => "Accepted",
            204 => "No Content",
            206 => "Partial Content",
            303 => "See Other",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            413 => "Payload Too Large",
            415 => "Unsupported Media Type",
            416 => "Range Not Satisfiable",
            421 => "Misdirected Request",
            422 => "Unprocessable Content",
            429 => "Too Many Requests",
            503 => "Service Unavailable",
            _ => "Internal Server Error",
        };
        let mut head = format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {length}\r\nCache-Control: no-store\r\nConnection: close\r\n",
            self.status, self.content_type
        );
        for (name, value) in &self.headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        head.into_bytes()
    }

    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let Body::Bytes(body) = &self.body else {
            unreachable!("tests only serialise in-memory bodies")
        };
        let mut out = self.head(body.len() as u64);
        out.extend_from_slice(body);
        out
    }
}

enum RangeRequest {
    /// Absent, multi-range or malformed: answer with the whole file (RFC 9110 allows ignoring).
    Ignored,
    Unsatisfiable,
    Bytes(u64, u64),
}

fn parse_range(value: &str, size: u64) -> RangeRequest {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return RangeRequest::Ignored;
    };
    if spec.contains(',') {
        return RangeRequest::Ignored;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return RangeRequest::Ignored;
    };
    let (first, last) = (first.trim(), last.trim());
    if size == 0 {
        return RangeRequest::Unsatisfiable;
    }
    let range = if first.is_empty() {
        match last.parse::<u64>() {
            Ok(0) => return RangeRequest::Unsatisfiable,
            Ok(suffix) => (size.saturating_sub(suffix), size - 1),
            Err(_) => return RangeRequest::Ignored,
        }
    } else {
        let Ok(start) = first.parse::<u64>() else {
            return RangeRequest::Ignored;
        };
        let end = if last.is_empty() {
            size - 1
        } else {
            match last.parse::<u64>() {
                Ok(end) => end.min(size - 1),
                Err(_) => return RangeRequest::Ignored,
            }
        };
        (start, end)
    };
    if range.0 >= size || range.0 > range.1 {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Bytes(range.0, range.1)
}

pub(crate) fn write_response(
    mut stream: TcpStream,
    mut response: HttpResponse,
    extra_headers: &[(&'static str, String)],
) -> jikji_core::Result<()> {
    response.headers.extend(extra_headers.iter().cloned());
    let write_error = |source| jikji_core::io_error("<gui-write>", source);
    let length = match &response.body {
        Body::Bytes(body) => body.len() as u64,
        Body::File { len, .. } => *len,
    };
    stream
        .write_all(&response.head(length))
        .map_err(write_error)?;
    match response.body {
        Body::Bytes(body) => stream.write_all(&body).map_err(write_error),
        Body::File { file, len } => {
            // A client that stops reading (e.g. a seek in a video) closes the socket; that is
            // not a server error, so the copy result is ignored.
            let _ = std::io::copy(&mut file.take(len), &mut stream);
            Ok(())
        }
    }
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
pub(crate) fn query_values(query: &str, name: &str) -> Vec<String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then(|| percent_decode(value))
        })
        .collect()
}

pub(crate) fn query_flag(query: &str, name: &str) -> Option<bool> {
    query_value(query, name).map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
}

pub(crate) fn query_bool(query: &str, name: &str) -> bool {
    query_flag(query, name).unwrap_or(false)
}

fn read_headers<R: BufRead>(reader: &mut R) -> jikji_core::Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    let mut line = String::new();
    let mut total = 0;
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|source| jikji_core::io_error("<gui-header>", source))?;
        total += read;
        if read == 0 || line == "\r\n" || line == "\n" || total > MAX_HEADER_BYTES {
            return Ok(headers);
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
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

#[cfg(test)]
mod tests {
    use super::{HttpResponse, query_bool, query_flag};
    use serde_json::json;

    #[test]
    fn json_202_uses_accepted_reason() {
        let bytes = HttpResponse::json(202, json!({"ok": true})).to_bytes();
        let head = String::from_utf8_lossy(&bytes);
        assert!(
            head.starts_with("HTTP/1.1 202 Accepted\r\n"),
            "unexpected status line: {head:?}"
        );
    }

    #[test]
    fn json_500_keeps_internal_server_error_reason() {
        let bytes = HttpResponse::json(500, json!({"error": "x"})).to_bytes();
        let head = String::from_utf8_lossy(&bytes);
        assert!(
            head.starts_with("HTTP/1.1 500 Internal Server Error\r\n"),
            "unexpected status line: {head:?}"
        );
    }

    #[test]
    fn query_flag_accepts_on_and_defaults_cleanly() {
        assert_eq!(query_flag("dry_run=on", "dry_run"), Some(true));
        assert_eq!(query_flag("dry_run=0", "dry_run"), Some(false));
        assert_eq!(query_flag("", "dry_run"), None);
        assert!(query_bool("dry_run=on", "dry_run"));
        assert!(!query_bool("", "dry_run"));
    }
}
