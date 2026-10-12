//! Fail-closed admission of *untrusted* client correlation headers.
//! This is an ingress guard, not a replacement for the shared ORES lifecycle.
use axum::{
    body::Body,
    http::{HeaderMap, Request},
    middleware::Next,
    response::Response,
};

fn safe_request_id(value: &str) -> bool {
    // Do not accept free-form identifiers that might be caller-supplied tokens,
    // prompts, or source material and then leak them into logs.
    value.len() == 36
        && uuid::Uuid::parse_str(value)
            .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == value)
}

fn safe_traceparent(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 55 || &bytes[0..3] != b"00-" || bytes[35] != b'-' || bytes[52] != b'-' {
        return false;
    }
    if bytes[3..35].iter().all(|byte| *byte == b'0')
        || bytes[36..52].iter().all(|byte| *byte == b'0')
    {
        return false;
    }
    bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| matches!(index, 2 | 35 | 52) && *byte == b'-'
            || !matches!(index, 2 | 35 | 52)
                && (byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)))
}

/// Never forward third-party tracestate or arbitrary baggage into OTel logs.
/// The bounded telemetry stack will explicitly author safe baggage itself.
pub fn sanitize_correlation_headers(headers: &mut HeaderMap) {
    if !headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .is_some_and(safe_request_id)
    {
        headers.remove("x-request-id");
    }
    if !headers
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .is_some_and(safe_traceparent)
    {
        headers.remove("traceparent");
    }
    // Even a syntactically valid baggage entry can carry untrusted prompts,
    // secrets or private source. A future trusted-proxy contract may admit
    // selected otel.* entries; until then always strip before tracing.
    headers.remove("baggage");
    headers.remove("tracestate");
}

pub async fn sanitize_inbound_correlation(mut request: Request<Body>, next: Next) -> Response {
    sanitize_correlation_headers(request.headers_mut());
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::sanitize_correlation_headers;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn keeps_bounded_id_and_valid_w3c_traceparent_without_baggage() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("550e8400-e29b-41d4-a716-446655440000"));
        headers.insert("traceparent", HeaderValue::from_static(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        ));
        headers.insert("baggage", HeaderValue::from_static("otel.job_id=secret-prompt"));
        headers.insert("tracestate", HeaderValue::from_static("vendor=source-code"));
        sanitize_correlation_headers(&mut headers);
        assert_eq!(headers.get("x-request-id").unwrap(), "req_2026-10.ok");
        assert!(headers.contains_key("traceparent"));
        assert!(!headers.contains_key("baggage"));
        assert!(!headers.contains_key("tracestate"));
    }

    #[test]
    fn rejects_secret_or_oversize_request_ids_and_invalid_traceparents() {
        for bad in ["Bearer_private-secret", "prompt=source-code", "", "has spaces",
            "bad/token", "a-very-long-identifier-that-is-more-than-sixty-four-bytes-long-and-unsafe"] {
            let mut headers = HeaderMap::new();
            if !bad.is_empty() {
                headers.insert("x-request-id", HeaderValue::from_str(bad).unwrap());
            }
            headers.insert("traceparent", HeaderValue::from_static(
                "00-00000000000000000000000000000000-00f067aa0ba902b7-01"));
            headers.insert("baggage", HeaderValue::from_static(
                "private-prompt=top-secret-canary"));
            sanitize_correlation_headers(&mut headers);
            assert!(!headers.contains_key("traceparent"));
            assert!(!headers.contains_key("baggage"));
            // Only canonical UUID v4 IDs survive; the request-id layer
            // generates a fresh UUID when the supplied value is untrusted.
            assert_eq!(headers.contains_key("x-request-id"),
                false);
        }
    }

    #[test]
    fn rejects_traceparent_with_invalid_span_or_uppercase() {
        for bad in [
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("traceparent", HeaderValue::from_str(bad).unwrap());
            sanitize_correlation_headers(&mut headers);
            assert!(!headers.contains_key("traceparent"));
        }
    }
}
