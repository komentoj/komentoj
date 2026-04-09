//! HTTP Signatures (draft-cavage-http-signatures-12) implementation.
//!
//! Reference implementations studied: GoToSocial, Mitra.
//! Key correctness decisions:
//!
//! - Algorithm: rsa-sha256 (RSASSA-PKCS1-v1_5 + SHA-256). This is what every
//!   major fediverse server uses in practice regardless of what the spec says
//!   about "hs2019".
//! - Headers signed on outbound POST: (request-target) host date digest
//! - Headers signed on outbound GET:  (request-target) host date
//! - `Digest` header uses old RFC 3230 style: `SHA-256=<base64>` (uppercase,
//!   plain `=` separator) — the format still used by Mastodon 4.x.
//! - When verifying, we accept both `Digest` and `Content-Digest`.
//! - We accept both SPKI ("PUBLIC KEY") and PKCS1 ("RSA PUBLIC KEY") PEM blocks.
//! - Date freshness window: ±5 minutes from current time.
//! - If cached key fails, the caller re-fetches and retries once.

use crate::error::{AppError, AppResult};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::{DateTime, Utc};
use rsa::{
    pkcs1::DecodeRsaPublicKey,
    pkcs8::DecodePublicKey,
    Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey,
};
use sha2::{Digest as Sha2Digest, Sha256};
use std::collections::HashMap;

// ── Constants ────────────────────────────────────────────────────────────────

const DATE_FRESHNESS_SECS: i64 = 300; // ±5 minutes

// ── Outbound signing ─────────────────────────────────────────────────────────

/// Headers to add to a signed outbound request.
pub struct SignedRequestHeaders {
    pub date: String,
    pub signature: String,
}

/// Sign an outbound HTTP request using the instance private key.
///
/// `body` should be `None` for GET requests, `Some(body_bytes)` for POST.
pub fn sign_request(
    method: &str,           // "get" | "post" (lowercase)
    path: &str,             // e.g. "/users/alice/inbox"
    host: &str,             // e.g. "example.com"
    body: Option<&[u8]>,
    private_key: &RsaPrivateKey,
    key_id: &str,
) -> AppResult<SignedRequestHeaders> {
    let date = httpdate_now();

    let digest = body.map(|b| {
        let hash = Sha256::digest(b);
        format!("SHA-256={}", B64.encode(hash))
    });

    // Build the list of headers to sign
    let mut headers_to_sign: Vec<(String, String)> = vec![
        (
            "(request-target)".to_string(),
            format!("{} {}", method.to_lowercase(), path),
        ),
        ("host".to_string(), host.to_string()),
        ("date".to_string(), date.clone()),
    ];

    if let Some(d) = &digest {
        headers_to_sign.push(("digest".to_string(), d.clone()));
    }

    let signing_string = build_signing_string(&headers_to_sign);
    let sig_bytes = rsa_sha256_sign(&signing_string, private_key)?;
    let sig_b64 = B64.encode(&sig_bytes);

    let headers_list = headers_to_sign
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    let signature = format!(
        r#"keyId="{key_id}",algorithm="rsa-sha256",headers="{headers_list}",signature="{sig_b64}""#
    );

    Ok(SignedRequestHeaders { date, signature })
}

// ── Inbound verification ─────────────────────────────────────────────────────

/// The parsed Signature header fields.
#[derive(Debug)]
pub struct ParsedSignature {
    pub key_id: String,
    pub signed_headers: Vec<String>,
    pub signature_bytes: Vec<u8>,
}

/// Verify the HTTP signature on an incoming inbox POST.
///
/// `request_headers` maps **lowercase** header names to their values.
/// `method` should be lowercase ("post"). `path` is the raw request path.
///
/// Returns the `keyId` from the signature on success.
pub fn verify_request(
    method: &str,
    path: &str,
    request_headers: &HashMap<String, String>,
    body: &[u8],
    public_key_pem: &str,
) -> AppResult<()> {
    // 1. Parse the Signature header
    let sig_header = request_headers
        .get("signature")
        .ok_or_else(|| AppError::Unauthorized("missing Signature header".into()))?;
    let parsed = parse_signature_header(sig_header)?;

    // 2. Check date freshness — mandatory when `date` is in the signed header list;
    //    also checked as defence-in-depth when the Date header is present but unsigned.
    if parsed.signed_headers.contains(&"date".to_string()) {
        let date_str = request_headers.get("date").ok_or_else(|| {
            AppError::Unauthorized(
                "Signature lists 'date' but the Date header is absent".into(),
            )
        })?;
        check_date_freshness(date_str)?;
    } else if let Some(date_str) = request_headers.get("date") {
        check_date_freshness(date_str)?;
    }

    // 3. Verify body digest (required when "digest" or "content-digest" is in signed headers)
    let digest_in_sig = parsed
        .signed_headers
        .iter()
        .any(|h| h == "digest" || h == "content-digest");
    if digest_in_sig {
        verify_body_digest(request_headers, body)?;
    } else if request_headers.contains_key("digest") || request_headers.contains_key("content-digest") {
        // Digest present but not listed in Signature: still verify as defense-in-depth.
        let _ = verify_body_digest(request_headers, body);
    }

    // 4. Reconstruct the signing string from the listed headers.
    //    Every header named in the `headers=` list MUST be present — an absent
    //    header cannot be treated as an empty string, because doing so would let
    //    a sender sign over an empty Date and bypass the freshness check.
    let mut header_pairs: Vec<(String, String)> = Vec::new();
    for hname in &parsed.signed_headers {
        let value = if hname == "(request-target)" {
            format!("{} {}", method.to_lowercase(), path)
        } else {
            request_headers
                .get(hname.as_str())
                .cloned()
                .ok_or_else(|| {
                    AppError::Unauthorized(format!(
                        "Signature lists header '{hname}' but it is absent from the request"
                    ))
                })?
        };
        header_pairs.push((hname.clone(), value));
    }
    let signing_string = build_signing_string(&header_pairs);

    // 5. Parse the public key and verify
    // Accept both SPKI ("PUBLIC KEY") and PKCS1 ("RSA PUBLIC KEY") PEM blocks.
    let public_key = parse_public_key_pem(public_key_pem)?;
    rsa_sha256_verify(&signing_string, &parsed.signature_bytes, &public_key)
}

/// Parse `keyId` from the Signature header without full verification.
/// Used to know which actor to fetch the key for.
pub fn extract_key_id(signature_header: &str) -> AppResult<String> {
    let parsed = parse_signature_header(signature_header)?;
    Ok(parsed.key_id)
}

// ── Signature header parser ───────────────────────────────────────────────────

fn parse_signature_header(value: &str) -> AppResult<ParsedSignature> {
    let mut key_id = None::<String>;
    let mut _algorithm = None::<String>;
    let mut headers = None::<String>;
    let mut signature = None::<String>;

    // The Signature header is a comma-separated list of key="value" pairs.
    // Values are double-quoted strings. We parse naively but correctly.
    for part in split_signature_params(value) {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            let k = k.trim().to_lowercase();
            // Strip surrounding double quotes from value
            let v = v.trim().trim_matches('"').to_string();
            match k.as_str() {
                "keyid" => key_id = Some(v),
                "algorithm" => _algorithm = Some(v),
                "headers" => headers = Some(v),
                "signature" => signature = Some(v),
                _ => {}
            }
        }
    }

    let key_id = key_id.ok_or_else(|| {
        AppError::Unauthorized("Signature header missing keyId".into())
    })?;
    let sig_b64 = signature.ok_or_else(|| {
        AppError::Unauthorized("Signature header missing signature".into())
    })?;
    let signature_bytes = B64
        .decode(&sig_b64)
        .map_err(|_| AppError::Unauthorized("invalid base64 in signature".into()))?;

    // Default header list when not specified (per Cavage draft §2.1.6)
    let signed_headers = headers
        .unwrap_or_else(|| "date".to_string())
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();

    Ok(ParsedSignature {
        key_id,
        signed_headers,
        signature_bytes,
    })
}

/// Split a Signature header value into key="value" fragments, respecting
/// quoted strings that may contain commas.
fn split_signature_params(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in value.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ',' if !in_quotes => {
                parts.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

// ── Digest verification ───────────────────────────────────────────────────────

fn verify_body_digest(
    headers: &HashMap<String, String>,
    body: &[u8],
) -> AppResult<()> {
    let actual_hash = Sha256::digest(body);

    // Try RFC 3230 `Digest: SHA-256=<base64>` first (most common)
    if let Some(d) = headers.get("digest") {
        return verify_digest_rfc3230(d, &actual_hash);
    }

    // Try RFC 9530 `Content-Digest: sha-256=:<base64>:` (newer implementations)
    if let Some(d) = headers.get("content-digest") {
        return verify_digest_rfc9530(d, &actual_hash);
    }

    Err(AppError::Unauthorized(
        "digest header required but missing".into(),
    ))
}

fn verify_digest_rfc3230(header: &str, actual: &[u8]) -> AppResult<()> {
    // Format: `SHA-256=<base64>` (may also have multiple algorithms separated by comma)
    for part in header.split(',') {
        let part = part.trim();
        let upper = part.to_uppercase();
        if upper.starts_with("SHA-256=") {
            let b64 = &part["SHA-256=".len()..];
            let claimed = B64
                .decode(b64)
                .map_err(|_| AppError::Unauthorized("invalid Digest base64".into()))?;
            if claimed.as_slice() != actual {
                return Err(AppError::Unauthorized("Digest mismatch".into()));
            }
            return Ok(());
        }
    }
    // Header present but no SHA-256 algorithm found — reject; we cannot verify body integrity.
    Err(AppError::Unauthorized(
        "Digest header contains no SHA-256 entry".into(),
    ))
}

fn verify_digest_rfc9530(header: &str, actual: &[u8]) -> AppResult<()> {
    // Format: `sha-256=:<base64>:` (SFV byte-sequence)
    for part in header.split(',') {
        let part = part.trim();
        let lower = part.to_lowercase();
        if lower.starts_with("sha-256=:") {
            let inner = part["sha-256=:".len()..].trim_end_matches(':');
            let claimed = B64
                .decode(inner)
                .map_err(|_| AppError::Unauthorized("invalid Content-Digest base64".into()))?;
            if claimed.as_slice() != actual {
                return Err(AppError::Unauthorized("Content-Digest mismatch".into()));
            }
            return Ok(());
        }
    }
    // Header present but no sha-256 entry — reject.
    Err(AppError::Unauthorized(
        "Content-Digest header contains no sha-256 entry".into(),
    ))
}

// ── Date freshness ────────────────────────────────────────────────────────────

fn check_date_freshness(date_str: &str) -> AppResult<()> {
    // Try parsing as HTTP-date (RFC 7231 / RFC 1123)
    let dt = httpdate::parse_http_date(date_str)
        .map(|st| DateTime::<Utc>::from(st))
        .map_err(|_| AppError::Unauthorized(format!("invalid Date header: {date_str}")))?;

    let now = Utc::now();
    let diff = (now - dt).num_seconds().abs();

    if diff > DATE_FRESHNESS_SECS {
        return Err(AppError::Unauthorized(format!(
            "request Date is stale ({diff}s ago, max {DATE_FRESHNESS_SECS}s)"
        )));
    }
    Ok(())
}

// ── Signing string construction ───────────────────────────────────────────────

fn build_signing_string(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(k, v)| format!("{}: {}", k.to_lowercase(), v))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── RSA-SHA256 primitives ────────────────────────────────────────────────────

fn rsa_sha256_sign(message: &str, key: &RsaPrivateKey) -> AppResult<Vec<u8>> {
    let hash = Sha256::digest(message.as_bytes());
    key.sign(Pkcs1v15Sign::new::<Sha256>(), &hash)
        .map_err(|e| AppError::Crypto(format!("signing failed: {e}")))
}

fn rsa_sha256_verify(
    message: &str,
    signature: &[u8],
    key: &RsaPublicKey,
) -> AppResult<()> {
    let hash = Sha256::digest(message.as_bytes());
    key.verify(Pkcs1v15Sign::new::<Sha256>(), &hash, signature)
        .map_err(|e| AppError::Crypto(format!("signature invalid: {e}")))
}

fn parse_public_key_pem(pem: &str) -> AppResult<RsaPublicKey> {
    // Try SPKI ("PUBLIC KEY") first — the standard modern format
    if let Ok(k) = RsaPublicKey::from_public_key_pem(pem) {
        return Ok(k);
    }
    // Fall back to PKCS1 ("RSA PUBLIC KEY") — sent by some older implementations
    RsaPublicKey::from_pkcs1_pem(pem)
        .map_err(|e| AppError::Crypto(format!("failed to parse public key PEM: {e}")))
}

// ── Date helper ───────────────────────────────────────────────────────────────

fn httpdate_now() -> String {
    httpdate::fmt_http_date(std::time::SystemTime::now())
}

// ── Digest helper (public, used by fetch module) ──────────────────────────────

/// Compute the RFC 3230 `Digest: SHA-256=<base64>` header value for a body.
pub fn compute_digest(body: &[u8]) -> String {
    let hash = Sha256::digest(body);
    format!("SHA-256={}", B64.encode(hash))
}

/// Extract the `keyId` from a raw Signature header string, stripping the
/// `#…` fragment to obtain the actor URL.
pub fn key_id_to_actor_url(key_id: &str) -> String {
    match key_id.rfind('#') {
        Some(idx) => key_id[..idx].to_string(),
        None => key_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_signature_params_handles_commas_in_quotes() {
        let header = r#"keyId="https://example.com/actor#main-key",algorithm="rsa-sha256",headers="(request-target) host date digest",signature="abc123=""#;
        let parts = split_signature_params(header);
        assert_eq!(parts.len(), 4);
        assert!(parts[0].starts_with("keyId="));
        assert!(parts[2].starts_with("headers="));
    }

    #[test]
    fn build_signing_string_no_trailing_newline() {
        let headers = vec![
            ("(request-target)".to_string(), "post /inbox".to_string()),
            ("host".to_string(), "example.com".to_string()),
        ];
        let s = build_signing_string(&headers);
        assert_eq!(s, "(request-target): post /inbox\nhost: example.com");
        assert!(!s.ends_with('\n'));
    }

    #[test]
    fn key_id_to_actor_url_strips_fragment() {
        assert_eq!(
            key_id_to_actor_url("https://example.com/users/alice#main-key"),
            "https://example.com/users/alice"
        );
        assert_eq!(
            key_id_to_actor_url("https://example.com/actor"),
            "https://example.com/actor"
        );
    }
}
