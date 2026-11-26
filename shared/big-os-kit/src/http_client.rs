// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "http-client feature owns blocking system curl subprocess calls"
)]

//! Blocking HTTP client policy backed by the system `curl` command.
//!
//! The backend centralizes outbound HTTP without linking Rust TLS stacks into
//! every consumer. `curl` is executed with argv-only arguments, explicit
//! timeouts, bounded reads, and validated header values.

use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Duration;

mod header_config;
use header_config::CurlRequestConfig;
pub use header_config::validate_header_name;

const CURL: &str = "curl";
const USER_AGENT: &str = "BigLinuxRustApp/0.1 (https://github.com/biglinux/big)";
/// Browser-like user agent for sites that reject generic clients.
pub(crate) const BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120.0.0.0";
const TIMEOUT_SECS: u64 = 15;
const FAST_TIMEOUT_SECS: u64 = 5;
const MAX_TEXT_BODY_BYTES: u64 = 5 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 128 * 1024;
const DEFAULT_MAX_REDIRECTS: u8 = 10;
/// curl's exit code when the host name does not resolve (`man curl`).
const CURL_COULDNT_RESOLVE_HOST: i32 = 6;
const RETRY_DELAY: Duration = Duration::from_millis(500);

/// Why a request through `curl` got no usable answer.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// The URL or a header holds a character curl's config file cannot carry.
    #[error("{field} contains an unsafe curl config character")]
    UnsafeValue {
        /// What held it: `URL` or `HTTP header`.
        field: &'static str,
    },
    /// A header name that is empty or holds a separator.
    #[error("HTTP header name is invalid")]
    HeaderName,
    /// The URL holds a NUL byte, which no argv carries.
    #[error("URL contains a NUL byte")]
    NulInUrl,
    /// The anonymous file the headers travel in could not be made.
    #[error("{step} curl request config: {error}")]
    RequestConfig {
        /// What was being done to it.
        step: &'static str,
        /// What the system said.
        error: std::io::Error,
    },
    /// `curl` did not start.
    #[error("spawn curl: {0}")]
    Spawn(std::io::Error),
    /// A pipe to `curl` was not there to take.
    #[error("curl {0} unavailable")]
    Pipe(&'static str),
    /// The request body did not reach `curl`.
    #[error("write curl stdin: {0}")]
    WriteBody(std::io::Error),
    /// Reading the answer failed.
    #[error("read curl {part}: {error}")]
    Read {
        /// `headers` or `body`.
        part: &'static str,
        /// What the system said.
        error: std::io::Error,
    },
    /// `curl` closed its output before any header, without saying why.
    #[error("curl ended before HTTP headers")]
    HeadersMissing,
    /// The headers ran past their size cap.
    #[error("HTTP headers exceeded {MAX_HEADER_BYTES} bytes cap")]
    HeadersTooLarge,
    /// No status line among the headers.
    #[error("missing HTTP status from curl headers")]
    NoStatus,
    /// The body ran past the caller's cap.
    #[error("response body exceeded {0} bytes cap")]
    BodyTooLarge(u64),
    /// Waiting for `curl` to end failed.
    #[error("wait curl: {0}")]
    Wait(std::io::Error),
    /// `curl` failed before an answer, with its exit code. `man curl` names
    /// each: 6 the host was not found, 7 the connection failed, 28 the time ran
    /// out.
    #[error("curl exited with code {0}")]
    CurlExited(i32),
    /// `curl` ended on a signal.
    #[error("curl exited with status {0}")]
    CurlKilled(std::process::ExitStatus),
}

/// Response body plus HTTP metadata.
#[derive(Debug, Clone)]
pub struct HttpResponseBytes {
    /// HTTP status code after redirects.
    pub status: u16,
    /// Response Content-Type header, if present.
    pub content_type: Option<String>,
    /// Response bytes, bounded by the caller-provided cap.
    pub bytes: Vec<u8>,
}

/// Streaming response plus metadata.
pub struct HttpStreamResponse {
    /// HTTP status code after redirects.
    pub status: u16,
    /// Response Content-Type header, if present.
    pub content_type: Option<String>,
    /// Content-Length header, if present and parseable.
    pub content_length: Option<u64>,
    /// Streaming body reader. Callers impose their semantic cap.
    pub reader: Box<dyn Read>,
}

/// How long a request may take.
#[derive(Debug, Clone, Copy)]
enum Deadline {
    /// The whole request, for an answer read into memory.
    Total(Duration),
    /// Each wait for more bytes once connected, for a download of any length.
    Stall(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpMethod {
    Get,
    Post,
}

struct CurlHeaders {
    status: u16,
    content_type: Option<String>,
    content_length: Option<u64>,
}

struct CurlBodyReader {
    /// Killed and reaped when the body reader drops; taken once the body
    /// ends, when its exit says whether the body was whole.
    process: Option<CurlProcess>,
    stdout: BufReader<ChildStdout>,
}

impl Read for CurlBodyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.stdout.read(buf)?;
        // A stalled or reset transfer also ends the output: only curl's exit
        // tells it from the end of the body.
        if read == 0
            && !buf.is_empty()
            && let Some(process) = self.process.take()
        {
            wait_for_success(process).map_err(std::io::Error::other)?;
        }
        Ok(read)
    }
}

/// Kills and reaps curl on drop, so every early return leaves no zombie.
struct CurlProcess {
    child: Child,
    request_config: Option<CurlRequestConfig>,
}

impl Drop for CurlProcess {
    fn drop(&mut self) {
        self.kill_and_wait();
    }
}

impl CurlProcess {
    fn stdout_take(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    fn stdin_take(&mut self) -> Option<std::process::ChildStdin> {
        self.child.stdin.take()
    }

    /// `Child` keeps the reaped status, so killing after `wait` signals nothing.
    fn kill_and_wait(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        drop(self.request_config.take());
    }

    fn wait(&mut self) -> Result<std::process::ExitStatus, HttpError> {
        let status = self.child.wait().map_err(HttpError::Wait)?;
        drop(self.request_config.take());
        Ok(status)
    }
}

/// Per-request headers for callers that need browser/API compatibility.
#[derive(Debug, Clone, Default)]
pub struct RequestHeaders<'a> {
    /// Override for the `User-Agent` header; empty falls back to the default.
    pub user_agent: &'a str,
    /// Override for the `Referer` header; empty omits the header.
    pub referer: &'a str,
}

impl<'a> RequestHeaders<'a> {
    /// Browser-like headers for sites that reject generic HTTP clients.
    #[must_use]
    pub fn browser() -> Self {
        Self {
            user_agent: BROWSER_USER_AGENT,
            referer: "",
        }
    }

    /// Return a copy with a User-Agent override.
    #[must_use]
    pub fn user_agent(mut self, value: &'a str) -> Self {
        self.user_agent = value;
        self
    }

    fn pairs(&self) -> Vec<(&str, &str)> {
        let mut pairs = Vec::with_capacity(2);
        if !self.user_agent.is_empty() {
            pairs.push(("User-Agent", self.user_agent));
        }
        if !self.referer.is_empty() {
            pairs.push(("Referer", self.referer));
        }
        pairs
    }
}

/// GET with caller-provided headers.
#[must_use]
pub fn http_get_with_headers(url: &str, headers: &RequestHeaders<'_>) -> Option<String> {
    http_get_text_with_headers(url, headers, &[], Duration::from_secs(TIMEOUT_SECS), true)
}

/// GET text as [`http_get_with_headers`] does, trying once more after 500 ms
/// when the server answers 5xx or the request fails before an answer, but not
/// when the host name does not resolve. `extra_headers`, such as `("Accept",
/// "application/json")`, go to every redirect target.
#[must_use]
pub fn http_get_with_retry(url: &str, extra_headers: &[(&str, &str)]) -> Option<String> {
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(RETRY_DELAY);
        }
        match curl_bytes(
            HttpMethod::Get,
            url,
            &RequestHeaders::default(),
            extra_headers,
            None,
            MAX_TEXT_BODY_BYTES,
            Deadline::Total(Duration::from_secs(TIMEOUT_SECS)),
            true,
        ) {
            Ok(response) if response.status < 500 => return text_body(response),
            Err(HttpError::CurlExited(CURL_COULDNT_RESOLVE_HOST)) => return None,
            Ok(_) | Err(_) => {}
        }
    }
    None
}

/// GET a response into memory with a caller-owned body cap. Redirects are
/// followed unless `headers` carries a `Referer`, which a provider may check
/// and which must not reach another host.
///
/// # Errors
///
/// [`HttpError`] when the URL or a header cannot reach `curl`, `curl` does not
/// start or fails before an answer, or the body passes `max_bytes`. An error
/// status is an answer, in [`HttpResponseBytes::status`].
pub fn http_get_bytes_capped(
    url: &str,
    headers: &RequestHeaders<'_>,
    max_bytes: u64,
    timeout: Duration,
) -> Result<HttpResponseBytes, HttpError> {
    curl_bytes(
        HttpMethod::Get,
        url,
        headers,
        &[],
        None,
        max_bytes,
        Deadline::Total(timeout),
        headers.referer.is_empty(),
    )
}

/// GET a response to read as it arrives, for a download of any length: after
/// connecting, it fails only once no byte arrives for `stall_timeout`, which
/// curl measures in whole seconds. Redirects as [`http_get_bytes_capped`].
///
/// # Errors
///
/// [`HttpError`] when the URL or a header cannot reach `curl`, `curl` does not
/// start or fails before an answer, or its headers are missing or too large. An
/// error status is an answer.
pub fn http_get_stream(
    url: &str,
    headers: &RequestHeaders<'_>,
    stall_timeout: Duration,
) -> Result<HttpStreamResponse, HttpError> {
    curl_stream(
        HttpMethod::Get,
        url,
        headers,
        &[],
        None,
        Deadline::Stall(stall_timeout),
        headers.referer.is_empty(),
    )
}

/// GET a response into memory with caller-owned body cap. Redirects are
/// disabled because curl sends the extra headers, such as API keys, to every
/// redirect target.
///
/// # Errors
///
/// [`HttpError`] when the URL or a header cannot reach `curl`, `curl` does not
/// start or fails before an answer, or the body passes `max_bytes`. An error
/// status is an answer, in [`HttpResponseBytes::status`].
pub fn http_get_bytes_capped_with_extra_headers(
    url: &str,
    headers: &RequestHeaders<'_>,
    extra_headers: &[(&str, &str)],
    max_bytes: u64,
    timeout: Duration,
) -> Result<HttpResponseBytes, HttpError> {
    curl_bytes(
        HttpMethod::Get,
        url,
        headers,
        extra_headers,
        None,
        max_bytes,
        Deadline::Total(timeout),
        false,
    )
}

/// POST JSON and return a streaming response. Redirects are disabled because
/// callers commonly attach API keys in headers.
///
/// # Errors
///
/// [`HttpError`] when the URL or a header cannot reach `curl`, `curl` does not
/// start or fails before an answer, the body does not reach it, or its headers
/// are missing or too large. An error status is an answer.
pub fn http_post_json_stream_with_extra_headers(
    url: &str,
    json_body: &str,
    headers: &RequestHeaders<'_>,
    extra_headers: &[(&str, &str)],
    timeout: Duration,
) -> Result<HttpStreamResponse, HttpError> {
    let mut all_headers = vec![("Content-Type", "application/json")];
    all_headers.extend_from_slice(extra_headers);
    curl_stream(
        HttpMethod::Post,
        url,
        headers,
        &all_headers,
        Some(json_body.as_bytes()),
        Deadline::Total(timeout),
        false,
    )
}

/// Read a stream into memory, refusing to exceed `max_bytes`.
///
/// # Errors
///
/// [`HttpError::Read`] when reading fails, [`HttpError::BodyTooLarge`] past the cap.
pub fn read_stream_capped(reader: &mut dyn Read, max_bytes: u64) -> Result<Vec<u8>, HttpError> {
    read_capped(reader, max_bytes)
}

fn http_get_text_with_headers(
    url: &str,
    headers: &RequestHeaders<'_>,
    extra_headers: &[(&str, &str)],
    timeout: Duration,
    follow_redirects: bool,
) -> Option<String> {
    let response = curl_bytes(
        HttpMethod::Get,
        url,
        headers,
        extra_headers,
        None,
        MAX_TEXT_BODY_BYTES,
        Deadline::Total(timeout),
        follow_redirects,
    )
    .ok()?;
    text_body(response)
}

/// The body of a 200 answer, when it is UTF-8.
fn text_body(response: HttpResponseBytes) -> Option<String> {
    (response.status == 200)
        .then(|| String::from_utf8(response.bytes).ok())
        .flatten()
}

fn curl_bytes(
    method: HttpMethod,
    url: &str,
    headers: &RequestHeaders<'_>,
    extra_headers: &[(&str, &str)],
    body: Option<&[u8]>,
    max_bytes: u64,
    deadline: Deadline,
    follow_redirects: bool,
) -> Result<HttpResponseBytes, HttpError> {
    let mut process = spawn_curl(
        method,
        url,
        headers,
        extra_headers,
        body,
        deadline,
        follow_redirects,
    )?;
    if let Some(body) = body {
        write_request_body(&mut process, body)?;
    }
    let mut stdout = BufReader::new(process.stdout_take().ok_or(HttpError::Pipe("stdout"))?);
    let parsed_headers = match read_final_headers(&mut stdout, follow_redirects) {
        Ok(headers) => headers,
        Err(error) => return Err(header_failure(process, error)),
    };
    let bytes = read_capped(&mut stdout, max_bytes)?;
    wait_for_success(process)?;
    Ok(HttpResponseBytes {
        status: parsed_headers.status,
        content_type: parsed_headers.content_type,
        bytes,
    })
}

fn curl_stream(
    method: HttpMethod,
    url: &str,
    headers: &RequestHeaders<'_>,
    extra_headers: &[(&str, &str)],
    body: Option<&[u8]>,
    deadline: Deadline,
    follow_redirects: bool,
) -> Result<HttpStreamResponse, HttpError> {
    let mut process = spawn_curl(
        method,
        url,
        headers,
        extra_headers,
        body,
        deadline,
        follow_redirects,
    )?;
    if let Some(body) = body {
        write_request_body(&mut process, body)?;
    }
    let mut stdout = BufReader::new(process.stdout_take().ok_or(HttpError::Pipe("stdout"))?);
    let parsed_headers = match read_final_headers(&mut stdout, follow_redirects) {
        Ok(headers) => headers,
        Err(error) => return Err(header_failure(process, error)),
    };
    Ok(HttpStreamResponse {
        status: parsed_headers.status,
        content_type: parsed_headers.content_type,
        content_length: parsed_headers.content_length,
        reader: Box::new(CurlBodyReader {
            process: Some(process),
            stdout,
        }),
    })
}

fn spawn_curl(
    method: HttpMethod,
    url: &str,
    headers: &RequestHeaders<'_>,
    extra_headers: &[(&str, &str)],
    body: Option<&[u8]>,
    deadline: Deadline,
    follow_redirects: bool,
) -> Result<CurlProcess, HttpError> {
    validate_url(url)?;
    let mut command = base_curl(deadline, headers.user_agent);
    restrict_protocols(&mut command, url);
    // Brackets and braces in a URL are its own characters, not curl's ranges.
    command.arg("--globoff");
    command.arg("--request").arg(match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
    });
    if follow_redirects {
        command
            .arg("--location")
            .arg("--max-redirs")
            .arg(DEFAULT_MAX_REDIRECTS.to_string());
    }
    let mut header_pairs = headers.pairs();
    for (name, value) in extra_headers
        .iter()
        .copied()
        .filter(|(_, value)| !value.is_empty())
    {
        header_pairs.push((name, value));
    }
    let request_config = CurlRequestConfig::write(url, &header_pairs)?;
    request_config.configure_command(&mut command);
    if body.is_some() {
        command.arg("--data-binary").arg("@-").stdin(Stdio::piped());
    }
    let child = command
        .stdout(Stdio::piped())
        .spawn()
        .map_err(HttpError::Spawn)?;
    Ok(CurlProcess {
        child,
        request_config: Some(request_config),
    })
}

/// HTTP and HTTPS only, redirects included: no `file://` or other scheme, and
/// an `https` request never follows a redirect down to plain `http`.
fn restrict_protocols(command: &mut Command, url: &str) {
    let secure = url
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"));
    command
        .arg("--proto")
        .arg("=http,https")
        .arg("--proto-redir")
        .arg(if secure { "=https" } else { "=http,https" });
}

fn base_curl(deadline: Deadline, user_agent: &str) -> Command {
    let mut command = Command::new(CURL);
    // First, or curl reads ~/.curlrc, whose `location` or `output` would
    // follow redirects with the caller's credentials or empty the pipe.
    command
        .arg("--disable")
        .arg("--silent")
        .arg("--show-error")
        // The headers come first on the output; behind a proxy, only the
        // server's, never the proxy's answer to CONNECT.
        .arg("--include")
        .arg("--suppress-connect-headers")
        .arg("--connect-timeout")
        .arg(FAST_TIMEOUT_SECS.to_string());
    match deadline {
        Deadline::Total(timeout) => {
            command
                .arg("--max-time")
                .arg(timeout.as_secs_f64().to_string());
        }
        // Below one byte a second for that long, curl gives up.
        Deadline::Stall(stall) => {
            command
                .arg("--speed-limit")
                .arg("1")
                .arg("--speed-time")
                .arg(stall.as_secs().max(1).to_string());
        }
    }
    command
        .arg("--user-agent")
        .arg(if user_agent.is_empty() {
            USER_AGENT
        } else {
            user_agent
        })
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn validate_url(url: &str) -> Result<(), HttpError> {
    if url.contains('\0') {
        return Err(HttpError::NulInUrl);
    }
    Ok(())
}

fn write_request_body(process: &mut CurlProcess, body: &[u8]) -> Result<(), HttpError> {
    let mut stdin = process.stdin_take().ok_or(HttpError::Pipe("stdin"))?;
    stdin.write_all(body).map_err(HttpError::WriteBody)
}

fn read_final_headers(
    stdout: &mut impl Read,
    follow_redirects: bool,
) -> Result<CurlHeaders, HttpError> {
    loop {
        let headers = read_one_header_block(stdout)?;
        let status = parse_status(&headers)?;
        let location = parse_header_value(&headers, "location");
        if (100..200).contains(&status)
            || (follow_redirects && (300..400).contains(&status) && location.is_some())
        {
            continue;
        }
        return Ok(CurlHeaders {
            status,
            content_type: parse_header_value(&headers, "content-type"),
            content_length: parse_header_value(&headers, "content-length")
                .and_then(|value| value.parse::<u64>().ok()),
        });
    }
}

fn read_one_header_block(stdout: &mut impl Read) -> Result<Vec<u8>, HttpError> {
    let mut headers = Vec::new();
    let mut byte = [0_u8; 1];
    while headers.len() < MAX_HEADER_BYTES {
        let n = stdout.read(&mut byte).map_err(|error| HttpError::Read {
            part: "headers",
            error,
        })?;
        if n == 0 {
            return Err(HttpError::HeadersMissing);
        }
        headers.push(byte[0]);
        if headers.ends_with(b"\r\n\r\n") || headers.ends_with(b"\n\n") {
            return Ok(headers);
        }
    }
    Err(HttpError::HeadersTooLarge)
}

/// The header reader over bytes curl did not produce; for `cargo fuzz` only.
#[cfg(fuzzing)]
#[doc(hidden)]
pub fn fuzz_read_headers(mut input: &[u8]) {
    let _ = read_final_headers(&mut input, true);
}

fn parse_status(headers: &[u8]) -> Result<u16, HttpError> {
    let text = String::from_utf8_lossy(headers);
    text.lines()
        .filter_map(|line| line.strip_prefix("HTTP/"))
        .filter_map(|rest| rest.split_whitespace().nth(1))
        .filter_map(|code| code.parse::<u16>().ok())
        .next_back()
        .ok_or(HttpError::NoStatus)
}

fn parse_header_value(headers: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(headers);
    text.lines().find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        header_name
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

fn read_capped(reader: &mut dyn Read, max_bytes: u64) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    let mut limited = reader.take(max_bytes.saturating_add(1));
    limited
        .read_to_end(&mut body)
        .map_err(|error| HttpError::Read {
            part: "body",
            error,
        })?;
    if body.len() as u64 > max_bytes {
        return Err(HttpError::BodyTooLarge(max_bytes));
    }
    Ok(body)
}

/// When curl closed its output before any header, it is exiting and its exit
/// code names the cause (6 is DNS); any other header error kills it on drop.
fn header_failure(process: CurlProcess, error: HttpError) -> HttpError {
    if matches!(error, HttpError::HeadersMissing)
        && let Err(exit) = wait_for_success(process)
    {
        return exit;
    }
    error
}

fn wait_for_success(mut process: CurlProcess) -> Result<(), HttpError> {
    let status = process.wait()?;
    if status.success() {
        Ok(())
    } else if let Some(code) = status.code() {
        Err(HttpError::CurlExited(code))
    } else {
        Err(HttpError::CurlKilled(status))
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn stream_cap_is_enforced() {
        let mut reader = &b"abcdef"[..];
        assert!(matches!(
            read_stream_capped(&mut reader, 3),
            Err(HttpError::BodyTooLarge(3))
        ));
    }

    #[test]
    fn read_cap_accepts_u64_max() {
        let mut reader = &b"abcdef"[..];
        assert_eq!(read_capped(&mut reader, u64::MAX).unwrap(), b"abcdef");
    }

    fn arguments(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn protocol_policy_restricts_redirects() {
        let mut command = Command::new(CURL);
        restrict_protocols(&mut command, "http://example.com/");
        assert_eq!(
            arguments(&command),
            ["--proto", "=http,https", "--proto-redir", "=http,https"]
        );
        let mut command = Command::new(CURL);
        restrict_protocols(&mut command, "HTTPS://example.com/");
        assert_eq!(
            arguments(&command),
            ["--proto", "=http,https", "--proto-redir", "=https"]
        );
    }

    /// A stream has no total deadline, so a long download is not cut short.
    #[test]
    fn stream_deadline_is_a_stall_limit() {
        let total = arguments(&base_curl(Deadline::Total(Duration::from_secs(15)), ""));
        assert!(total.windows(2).any(|pair| pair == ["--max-time", "15"]));
        let stall = arguments(&base_curl(Deadline::Stall(Duration::from_secs(30)), ""));
        assert!(!stall.iter().any(|arg| arg == "--max-time"));
        assert!(
            stall
                .windows(4)
                .any(|args| args == ["--speed-limit", "1", "--speed-time", "30"])
        );
    }

    /// Serve one canned answer per connection, in order, on a local port.
    fn serve(answers: Vec<&'static [u8]>) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for answer in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).unwrap();
                stream.write_all(answer).unwrap();
            }
        });
        (url, server)
    }

    #[test]
    fn retry_follows_a_server_error() {
        let (url, server) = serve(vec![
            b"HTTP/1.1 503 Busy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
        ]);
        assert_eq!(http_get_with_retry(&url, &[]).as_deref(), Some("ok"));
        server.join().unwrap();
        let (url, server) = serve(vec![
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ]);
        assert_eq!(http_get_with_retry(&url, &[]), None);
        server.join().unwrap();
    }

    /// A redirect is followed, unless a Referer would go to the other host.
    #[test]
    fn capped_get_keeps_a_referer_home() {
        let target = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nart",
        ]);
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            target.0
        )
        .leak()
        .as_bytes();
        let (url, server) = serve(vec![redirect, redirect]);
        let followed = http_get_bytes_capped(
            &url,
            &RequestHeaders::default(),
            1024,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            (followed.status, followed.bytes.as_slice()),
            (200, &b"art"[..])
        );
        let headers = RequestHeaders {
            referer: "http://provider.example/",
            ..RequestHeaders::default()
        };
        let kept = http_get_bytes_capped(&url, &headers, 1024, Duration::from_secs(5)).unwrap();
        assert_eq!(kept.status, 302);
        server.join().unwrap();
        target.1.join().unwrap();
    }

    #[test]
    fn refused_connection_reports_curl_exit_code() {
        let error = http_get_bytes_capped_with_extra_headers(
            "http://127.0.0.1:1/",
            &RequestHeaders::default(),
            &[],
            1024,
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(matches!(error, HttpError::CurlExited(7)), "{error}");
        assert_eq!(error.to_string(), "curl exited with code 7");
    }

    #[test]
    fn credentialed_get_refuses_redirects() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\n\
                      Content-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let response = http_get_bytes_capped_with_extra_headers(
            &url,
            &RequestHeaders::default(),
            &[("x-api-key", "secret")],
            1024,
            Duration::from_secs(5),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(response.status, 302);
    }

    /// A POST carries its caller's API key, so a redirect comes back as the
    /// answer instead of taking the key to another host.
    #[test]
    fn credentialed_post_refuses_redirects() {
        let (url, server) = serve(vec![
            b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/\r\n\
              Content-Length: 0\r\nConnection: close\r\n\r\n",
        ]);
        let response = http_post_json_stream_with_extra_headers(
            &url,
            "{}",
            &RequestHeaders::default(),
            &[("x-api-key", "secret")],
            Duration::from_secs(5),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(response.status, 307);
    }

    /// A stream follows redirects as the capped GET does: not with a Referer.
    #[test]
    fn stream_keeps_a_referer_home() {
        let (url, server) = serve(vec![
            b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\n\
              Content-Length: 0\r\nConnection: close\r\n\r\n",
        ]);
        let headers = RequestHeaders {
            referer: "http://provider.example/",
            ..RequestHeaders::default()
        };
        let response = http_get_stream(&url, &headers, Duration::from_secs(5)).unwrap();
        server.join().unwrap();
        assert_eq!(response.status, 302);
    }

    /// A name that does not resolve is not asked again: the retry would only
    /// add its delay. Timed against one attempt made after it, which a
    /// resolver's cache can only make faster.
    #[test]
    fn unresolved_host_fails_at_once() {
        let url = "http://unresolved.invalid/";
        let started = std::time::Instant::now();
        assert_eq!(http_get_with_retry(url, &[]), None);
        let retried = started.elapsed();
        let started = std::time::Instant::now();
        let single = http_get_bytes_capped(
            url,
            &RequestHeaders::default(),
            1024,
            Duration::from_secs(5),
        );
        let one_attempt = started.elapsed();
        assert!(
            matches!(
                single,
                Err(HttpError::CurlExited(CURL_COULDNT_RESOLVE_HOST))
            ),
            "{single:?}"
        );
        assert!(
            retried < one_attempt + RETRY_DELAY,
            "{retried:?} against one attempt of {one_attempt:?}"
        );
    }

    /// No argv carries a NUL, and curl's config file would cut the URL at it.
    #[test]
    fn nul_in_url_is_refused() {
        let error = http_get_bytes_capped(
            "http://127.0.0.1:1/\0x",
            &RequestHeaders::default(),
            1024,
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(matches!(error, HttpError::NulInUrl), "{error}");
    }

    /// The cap counts every byte of a block, its blank line included.
    #[test]
    fn header_block_stops_at_its_cap() {
        let block = |length: usize| {
            let mut block = vec![b'x'; length - 4];
            block.extend_from_slice(b"\r\n\r\n");
            block
        };
        let whole = block(MAX_HEADER_BYTES);
        assert_eq!(
            read_one_header_block(&mut whole.as_slice()).unwrap().len(),
            MAX_HEADER_BYTES
        );
        let over = block(MAX_HEADER_BYTES + 1);
        assert!(matches!(
            read_one_header_block(&mut over.as_slice()),
            Err(HttpError::HeadersTooLarge)
        ));
    }

    /// A body cut short ends in an error, not in a shorter body.
    #[test]
    fn cut_stream_reads_as_an_error() {
        let (url, server) = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort",
        ]);
        let mut response =
            http_get_stream(&url, &RequestHeaders::default(), Duration::from_secs(5)).unwrap();
        let read = read_stream_capped(response.reader.as_mut(), 1024);
        server.join().unwrap();
        assert!(read.is_err(), "{read:?}");
    }

    /// The server sees the path as asked: curl expands no `[1-3]` or `{a,b}`.
    #[test]
    fn url_brackets_reach_the_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/x[1-3]{{a,b}}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let line = String::from_utf8_lossy(&request[..read])
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{line}",
                line.len()
            );
            stream.write_all(answer.as_bytes()).unwrap();
        });
        let answer = http_get_with_headers(&url, &RequestHeaders::default());
        server.join().unwrap();
        assert_eq!(answer.as_deref(), Some("GET /x[1-3]{a,b} HTTP/1.1"));
    }

    /// curl reads `~/.curlrc` unless `--disable` comes first.
    #[test]
    fn curl_ignores_the_users_curlrc() {
        let command = base_curl(Deadline::Total(Duration::from_secs(1)), "");
        assert_eq!(
            arguments(&command).first().map(String::as_str),
            Some("--disable")
        );
    }

    /// Under `https_proxy` or `ALL_PROXY`, which curl honors, `--include`
    /// prints the proxy's `200 Connection established` first, and it would
    /// read as the server's answer.
    #[test]
    fn proxy_answer_stays_out_of_headers() {
        let command = base_curl(Deadline::Total(Duration::from_secs(1)), "");
        assert!(
            arguments(&command)
                .iter()
                .any(|argument| argument == "--suppress-connect-headers")
        );
    }
}
