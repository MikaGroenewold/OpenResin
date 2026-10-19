//! Just enough HTTP/1.1 for the fake printers (keep-alive, Content-Length and
//! chunked bodies, multipart/form-data).

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A request head: method, target, headers.
pub type Head = (String, String, Vec<(String, String)>);

/// Reads one request head; `buf` keeps bytes that arrived after it.
/// Returns `None` when the peer closed the connection.
pub async fn read_head<S: AsyncRead + Unpin>(s: &mut S, buf: &mut Vec<u8>) -> std::io::Result<Option<Head>> {
    loop {
        if let Some(end) = find(buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            buf.drain(..end + 4);
            let mut lines = head.lines();
            let first = lines.next().unwrap_or("");
            let mut parts = first.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect();
            return Ok(Some((method, path, headers)));
        }
        let mut tmp = [0u8; 64 * 1024];
        let n = s.read(&mut tmp).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

async fn fill<S: AsyncRead + Unpin>(s: &mut S, buf: &mut Vec<u8>, want: usize) -> std::io::Result<()> {
    let mut tmp = vec![0u8; 256 * 1024];
    while buf.len() < want {
        let n = s.read(&mut tmp).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Ok(())
}

async fn fill_until<S: AsyncRead + Unpin>(s: &mut S, buf: &mut Vec<u8>, needle: &[u8]) -> std::io::Result<usize> {
    loop {
        if let Some(i) = find(buf, needle) {
            return Ok(i);
        }
        let want = buf.len() + 1;
        fill(s, buf, want).await?;
    }
}

/// Reads the body that belongs to `headers`.
pub async fn read_body<S: AsyncRead + Unpin>(
    s: &mut S,
    buf: &mut Vec<u8>,
    headers: &[(String, String)],
) -> std::io::Result<Vec<u8>> {
    if header(headers, "Transfer-Encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        let mut body = Vec::new();
        loop {
            let eol = fill_until(s, buf, b"\r\n").await?;
            let size_line = String::from_utf8_lossy(&buf[..eol]).to_string();
            buf.drain(..eol + 2);
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("0").trim(), 16)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk size"))?;
            if size == 0 {
                let end = fill_until(s, buf, b"\r\n").await?;
                buf.drain(..end + 2);
                return Ok(body);
            }
            fill(s, buf, size + 2).await?;
            body.extend_from_slice(&buf[..size]);
            buf.drain(..size + 2);
        }
    }
    let len = header(headers, "Content-Length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    fill(s, buf, len).await?;
    Ok(buf.drain(..len).collect())
}

pub async fn read_request<S: AsyncRead + Unpin>(s: &mut S, buf: &mut Vec<u8>) -> std::io::Result<Option<Request>> {
    let Some((method, path, headers)) = read_head(s, buf).await? else {
        return Ok(None);
    };
    let body = read_body(s, buf, &headers).await?;
    Ok(Some(Request {
        method,
        path,
        headers,
        body,
    }))
}

pub async fn respond<S: AsyncWrite + Unpin>(
    s: &mut S,
    status: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).await?;
    // write in pieces so clients see a streamed body
    for chunk in body.chunks(64 * 1024) {
        s.write_all(chunk).await?;
    }
    s.flush().await
}

#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

/// Parses a `multipart/form-data` body.
pub fn parse_multipart(content_type: &str, body: &[u8]) -> Vec<Part> {
    let boundary = content_type
        .split(';')
        .find_map(|p| p.trim().strip_prefix("boundary="))
        .map(|b| b.trim_matches('"').to_string())
        .expect("multipart boundary");
    let delim = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut rest = body;
    // skip the preamble up to the first delimiter
    let Some(first) = find(rest, delim.as_bytes()) else {
        return parts;
    };
    rest = &rest[first + delim.len()..];
    loop {
        if rest.starts_with(b"--") {
            break; // closing delimiter
        }
        rest = rest.strip_prefix(b"\r\n").unwrap_or(rest);
        let Some(head_end) = find(rest, b"\r\n\r\n") else { break };
        let head = String::from_utf8_lossy(&rest[..head_end]).to_string();
        let data_start = head_end + 4;
        let end_marker = format!("\r\n{delim}");
        let Some(data_len) = find(&rest[data_start..], end_marker.as_bytes()) else {
            break;
        };
        let data = rest[data_start..data_start + data_len].to_vec();
        let mut name = String::new();
        let mut filename = None;
        let mut content_type = None;
        for line in head.lines() {
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("content-disposition:") {
                for p in line.split(';').map(str::trim) {
                    if let Some(v) = p.strip_prefix("name=") {
                        name = v.trim_matches('"').to_string();
                    } else if let Some(v) = p.strip_prefix("filename=") {
                        filename = Some(v.trim_matches('"').to_string());
                    }
                }
            } else if lower.starts_with("content-type:") {
                content_type = Some(line.split_once(':').unwrap().1.trim().to_string());
            }
        }
        parts.push(Part {
            name,
            filename,
            content_type,
            data,
        });
        rest = &rest[data_start + data_len + end_marker.len()..];
    }
    parts
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Some(Ok(v)) = s.get(i + 1..i + 3).map(|h| u8::from_str_radix(h, 16)) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses `bytes=a-b` / `bytes=a-` against a resource of `len` bytes.
/// Returns `Err(())` for unsatisfiable ranges.
pub fn parse_range(v: &str, len: u64) -> Result<(u64, u64), ()> {
    let r = v.trim().strip_prefix("bytes=").ok_or(())?;
    let (a, b) = r.split_once('-').ok_or(())?;
    let start: u64 = a.trim().parse().map_err(|_| ())?;
    if start >= len {
        return Err(());
    }
    let end = if b.trim().is_empty() {
        len - 1
    } else {
        b.trim().parse::<u64>().map_err(|_| ())?.min(len - 1)
    };
    if end < start {
        return Err(());
    }
    Ok((start, end))
}
