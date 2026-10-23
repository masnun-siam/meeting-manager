//! Downloads the secret iCalendar address under a fixed policy. Errors name the kind of
//! failure only: `FetchError` holds no strings, so the address cannot leak through it.

use std::fmt;
use std::time::Duration;

/// Where and how the fetch may go. Tests relax `https_only` for the loopback mock.
pub struct Policy {
    pub https_only: bool,
    pub max_redirects: u32,
    pub timeout: Duration,
    pub max_bytes: u64,
}

impl Policy {
    /// PRD FR-8: https only, at most 3 https redirects, 20 s, 50 MiB.
    pub const REAL: Policy = Policy {
        https_only: true,
        max_redirects: 3,
        timeout: Duration::from_secs(20),
        max_bytes: 50 * 1024 * 1024,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    Offline,
    Timeout,
    Http(u16),
    TooLarge,
    NotIcalendar,
    Tls,
    RedirectRefused,
    NotHttps,
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offline => f.write_str("offline"),
            Self::Timeout => f.write_str("timeout"),
            Self::Http(status) => write!(f, "http {status}"),
            Self::TooLarge => f.write_str("too large"),
            Self::NotIcalendar => f.write_str("not iCalendar"),
            Self::Tls => f.write_str("tls"),
            Self::RedirectRefused => f.write_str("redirect refused"),
            Self::NotHttps => f.write_str("not https"),
        }
    }
}

impl std::error::Error for FetchError {}

/// GETs `address` and returns the body if it is iCalendar text. Never logs the address.
pub fn fetch(address: &str, policy: &Policy) -> Result<String, FetchError> {
    if policy.https_only && !address.starts_with("https://") {
        return Err(FetchError::NotHttps);
    }
    let mut response = agent(policy).get(address).call().map_err(kind)?;
    let status = response.status().as_u16();
    if !(200..=299).contains(&status) {
        return Err(FetchError::Http(status));
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(policy.max_bytes)
        .read_to_vec()
        .map_err(kind)?;
    let text = String::from_utf8_lossy(&body).into_owned();
    if !text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with("BEGIN:VCALENDAR")
    {
        return Err(FetchError::NotIcalendar);
    }
    Ok(text)
}

fn agent(policy: &Policy) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(policy.timeout))
            .http_status_as_error(false)
            .https_only(policy.https_only)
            .max_redirects(policy.max_redirects)
            .build(),
    )
}

/// Maps by variant, never by text: `RequireHttpsOnly` and `BadUri` print the URL.
fn kind(e: ureq::Error) -> FetchError {
    match e {
        ureq::Error::Timeout(_) => FetchError::Timeout,
        ureq::Error::BodyExceedsLimit(_) => FetchError::TooLarge,
        ureq::Error::RequireHttpsOnly(_)
        | ureq::Error::TooManyRedirects
        | ureq::Error::RedirectFailed => FetchError::RedirectRefused,
        ureq::Error::Rustls(_) | ureq::Error::Tls(_) => FetchError::Tls,
        _ => FetchError::Offline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{mock_raw, stall};
    use std::time::Instant;

    const ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n";
    const SENTINEL: &str = "SECRET-TOKEN-123";

    fn loopback(max_bytes: u64) -> Policy {
        Policy {
            https_only: false,
            max_redirects: 3,
            timeout: Duration::from_millis(300),
            max_bytes,
        }
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn status(code: u16) -> String {
        format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    }

    fn redirects(n: usize) -> Vec<String> {
        let mut responses: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: {{base}}/{SENTINEL}/{i}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            })
            .collect();
        responses.push(ok(ICS));
        responses
    }

    fn run(responses: Vec<String>, policy: &Policy) -> Result<String, FetchError> {
        let (base, _rx) = mock_raw(responses);
        fetch(&format!("{base}/{SENTINEL}/cal.ics?k={SENTINEL}"), policy)
    }

    fn assert_no_address(e: &FetchError) {
        let chain = format!("{:#}", anyhow::Error::from(*e));
        for text in [e.to_string(), format!("{e:?}"), chain] {
            assert!(!text.contains(SENTINEL), "{text}");
            assert!(!text.contains("127.0.0.1"), "{text}");
        }
    }

    fn fails(result: Result<String, FetchError>, expected: FetchError, text: &str) {
        let e = result.unwrap_err();
        assert_eq!(e, expected);
        assert_eq!(e.to_string(), text);
        assert_no_address(&e);
    }

    #[test]
    fn real_policy_values() {
        let p = Policy::REAL;
        assert!(p.https_only);
        assert_eq!(p.max_redirects, 3);
        assert_eq!(p.timeout, Duration::from_secs(20));
        assert_eq!(p.max_bytes, 50 * 1024 * 1024);
    }

    #[test]
    fn real_agent_refuses_non_https_redirects() {
        let agent = agent(&Policy::REAL);
        assert!(agent.config().https_only());
        assert_eq!(agent.config().max_redirects(), 3);
        assert!(agent.config().max_redirects_will_error());
    }

    #[test]
    fn returns_icalendar_body() {
        assert_eq!(run(vec![ok(ICS)], &loopback(1024)).unwrap(), ICS);
    }

    #[test]
    fn accepts_bom_and_leading_whitespace() {
        let body = format!("\u{feff} \r\n{ICS}");
        assert_eq!(run(vec![ok(&body)], &loopback(1024)).unwrap(), body);
    }

    #[test]
    fn refuses_http_before_connecting() {
        let (base, rx) = mock_raw(vec![ok(ICS)]);
        let policy = Policy {
            https_only: true,
            ..loopback(1024)
        };
        fails(
            fetch(&format!("{base}/{SENTINEL}"), &policy),
            FetchError::NotHttps,
            "not https",
        );
        assert!(rx.try_recv().is_err(), "no request reached the server");
    }

    #[test]
    fn follows_three_redirects() {
        assert_eq!(run(redirects(3), &loopback(1024)).unwrap(), ICS);
    }

    #[test]
    fn refuses_a_fourth_redirect() {
        fails(
            run(redirects(4), &loopback(1024)),
            FetchError::RedirectRefused,
            "redirect refused",
        );
    }

    #[test]
    fn stalled_server_times_out() {
        let base = stall();
        let started = Instant::now();
        fails(
            fetch(&format!("{base}/{SENTINEL}"), &loopback(1024)),
            FetchError::Timeout,
            "timeout",
        );
        assert!(started.elapsed() < Duration::from_millis(1300));
    }

    #[test]
    fn body_over_the_cap_is_too_large() {
        let big = format!("{ICS}{}", "X".repeat(200));
        fails(
            run(vec![ok(&big)], &loopback(64)),
            FetchError::TooLarge,
            "too large",
        );
    }

    #[test]
    fn chunked_body_over_the_cap_is_too_large() {
        let chunk = "X".repeat(100);
        let chunked = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n64\r\n{chunk}\r\n64\r\n{chunk}\r\n0\r\n\r\n"
        );
        fails(
            run(vec![chunked], &loopback(64)),
            FetchError::TooLarge,
            "too large",
        );
    }

    #[test]
    fn non_2xx_is_http_status() {
        fails(
            run(vec![status(404)], &loopback(1024)),
            FetchError::Http(404),
            "http 404",
        );
        fails(
            run(vec![status(500)], &loopback(1024)),
            FetchError::Http(500),
            "http 500",
        );
    }

    #[test]
    fn html_or_empty_body_is_not_icalendar() {
        for body in ["<html>Sign in</html>", ""] {
            fails(
                run(vec![ok(body)], &loopback(1024)),
                FetchError::NotIcalendar,
                "not iCalendar",
            );
        }
    }

    #[test]
    fn refused_connection_is_offline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        fails(
            fetch(&format!("http://{addr}/{SENTINEL}"), &loopback(1024)),
            FetchError::Offline,
            "offline",
        );
    }

    #[test]
    fn every_kind_displays_without_the_address() {
        for e in [FetchError::Tls, FetchError::RedirectRefused] {
            assert_no_address(&e);
        }
        assert_eq!(FetchError::Tls.to_string(), "tls");
    }
}
