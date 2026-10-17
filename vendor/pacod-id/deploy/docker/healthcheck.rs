//! Health probe for container images that have no shell, curl or wget.
//!
//!     healthcheck http://127.0.0.1:9170/readyz    exit 0 when the answer is 2xx
//!     healthcheck tcp://127.0.0.1:9000            exit 0 when the port accepts a connection
//!
//! Standard library only. Compiled by `deploy/docker/pacod-id.Dockerfile` (stage `healthcheck`)
//! and copied to `/usr/local/bin/healthcheck` in both images.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::ExitCode;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

fn main() -> ExitCode {
    let Some(url) = std::env::args().nth(1) else {
        eprintln!("usage: healthcheck http://host:port/path | tcp://host:port");
        return ExitCode::from(2);
    };
    match probe(&url) {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("healthcheck {url}: {reason}");
            ExitCode::FAILURE
        }
    }
}

fn probe(url: &str) -> Result<(), String> {
    let (scheme, rest) = url.split_once("://").ok_or("the address needs a scheme, http:// or tcp://")?;
    if scheme != "http" && scheme != "tcp" {
        return Err(format!("unsupported scheme \"{scheme}\""));
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let host_port = if authority.contains(':') { authority.to_owned() } else { format!("{authority}:80") };
    let addr = host_port
        .to_socket_addrs()
        .map_err(|e| format!("resolving {host_port}: {e}"))?
        .next()
        .ok_or_else(|| format!("{host_port} has no address"))?;

    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| format!("connect: {e}"))?;
    if scheme == "tcp" {
        return Ok(());
    }

    stream.set_read_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: pacod-id-healthcheck\r\nConnection: close\r\n\r\n")
        .map_err(|e| format!("write: {e}"))?;

    // The status line is "HTTP/1.1 200 ...": twelve bytes are enough.
    let mut head = [0u8; 12];
    let mut filled = 0;
    while filled < head.len() {
        match stream.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    let line = std::str::from_utf8(&head[..filled]).map_err(|_| "the answer is not HTTP".to_owned())?;
    let status: u16 = line
        .strip_prefix("HTTP/1.")
        .and_then(|rest| rest.get(2..5))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("unexpected answer \"{line}\""))?;
    if (200..300).contains(&status) { Ok(()) } else { Err(format!("status {status}")) }
}
