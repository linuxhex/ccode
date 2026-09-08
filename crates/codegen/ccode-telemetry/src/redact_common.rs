//! Redaction helpers shared by the **internal** OTLP span pipeline
//! ([`crate::otel_layer`]) and the **external** customer-collector pipeline
//! ([`crate::external`]).
//!
//! Both pipelines are authoritative privacy chokepoints (see the crate
//! `AGENTS.md`); these helpers are the string-level scrubbing primitives they
//! share. Changes here affect every byte that leaves the process on either
//! pipeline.

use regex::{Regex, RegexSet};
use std::borrow::Cow;
use std::sync::LazyLock;

const REDACTED: &str = "[REDACTED_SECRET]";
const REDACTED_URL_VALUE: &str = "redacted";

// --- Secret-shape regexes ---

/// Vendor API keys with `sk-`/`sk_` prefixes and xAI (`xai-`) keys.
static API_KEY_PREFIX_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\b(?:sk[-_]|xai-)[A-Za-z0-9_-]{20,}"));

/// AWS long-term (`AKIA`) and temporary (`ASIA`) access-key IDs.
static AWS_ACCESS_KEY_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"));

/// GitHub PATs: classic + fine-grained.
static GITHUB_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\b(?:gh[opusr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})"));

/// GitLab (`glpat-`) and Slack (`xoxa-`/`xoxb-`/`xoxp-`/`xapp-`) tokens.
static VENDOR_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\b(?:glpat-|xox[abp]-|xapp-)[A-Za-z0-9-]{10,}"));

/// Google API keys (`AIza` + 35 chars).
static GOOGLE_API_KEY_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\bAIza[0-9A-Za-z_-]{35}"));

/// PEM private-key block.
static PEM_PRIVATE_KEY_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    compile(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----")
});

static BEARER_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?i)\bBearer\s+[A-Za-z0-9._\-]{16,}\b"));

/// Bare JWT.
static JWT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b"));

/// Secret assignment patterns (api_key=..., token: ..., etc.).
static SECRET_ASSIGNMENT_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r#"(?ix)
        \b(
            api[_-]?key
          | (?:access|refresh|id)[_-]token
          | token
          | secret
          | client[_-]secret
          | password
        )\b
        (\s*[:=]\s*)
        (["']?)
        [^\s"',&]{8,}
        "#,
    )
});

static SENSITIVE_QUERY_PARAMS: &[&str] = &[
    "access_token",
    "api_key",
    "assertion",
    "auth",
    "client_secret",
    "code",
    "code_verifier",
    "id_token",
    "key",
    "password",
    "refresh_token",
    "requested_token",
    "session_id",
    "state",
    "subject_token",
    "token",
];

static URL_REGEX: LazyLock<Regex> = LazyLock::new(|| compile(r#"https?://[^\s"'<>(){}\[\],;`]+"#));

static MATCH_ANY: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        API_KEY_PREFIX_REGEX.as_str(),
        AWS_ACCESS_KEY_REGEX.as_str(),
        GITHUB_TOKEN_REGEX.as_str(),
        VENDOR_TOKEN_REGEX.as_str(),
        GOOGLE_API_KEY_REGEX.as_str(),
        PEM_PRIVATE_KEY_REGEX.as_str(),
        BEARER_TOKEN_REGEX.as_str(),
        JWT_REGEX.as_str(),
        URL_REGEX.as_str(),
        SECRET_ASSIGNMENT_REGEX.as_str(),
    ])
    .expect("redact_secrets RegexSet")
});

// --- User path redaction ---

static HOME_DIR: LazyLock<Option<String>> = LazyLock::new(|| {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
});

static USERNAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut names: Vec<String> = Vec::new();
    for var in ["USERNAME", "USER"] {
        if let Ok(name) = std::env::var(var) {
            let trimmed = name.trim();
            if trimmed.len() >= 3 && !names.iter().any(|u| u.eq_ignore_ascii_case(trimmed)) {
                names.push(trimmed.to_owned());
            }
        }
    }
    names
});

fn is_segment_boundary(c: char) -> bool {
    !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
}

static HOME_ROOT_USER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"([/\\](?:Users|home)[/\\])([^/\\]+)"));

// --- Public API ---

/// Secret-shape then user-path scrub. Returns `Some` only when the input
/// changed (owned, so callers can overwrite in place).
pub(crate) fn redact_owned(input: &str) -> Option<String> {
    let secrets_redacted = redact_secrets(input);
    let path_redacted = redact_user_paths(secrets_redacted.as_ref());
    // If either pass produced an owned string, return it
    match path_redacted {
        Cow::Owned(s) => Some(s),
        Cow::Borrowed(_) => {
            // path_redacted didn't change anything; check if secrets did
            match secrets_redacted {
                Cow::Owned(s) => Some(s),
                Cow::Borrowed(_) => None,
            }
        }
    }
}

/// Scrub a string, returning the (possibly unchanged) owned value.
pub(crate) fn redact_to_owned(input: &str) -> String {
    redact_owned(input).unwrap_or_else(|| input.to_owned())
}

/// Redact secret shapes from a string.
pub(crate) fn redact_secrets(input: &str) -> Cow<'_, str> {
    if !MATCH_ANY.is_match(input) {
        return Cow::Borrowed(input);
    }
    let s = PEM_PRIVATE_KEY_REGEX.replace_all(input, REDACTED);
    let s = API_KEY_PREFIX_REGEX.replace_all(&s, REDACTED);
    let s = AWS_ACCESS_KEY_REGEX.replace_all(&s, REDACTED);
    let s = GITHUB_TOKEN_REGEX.replace_all(&s, REDACTED);
    let s = VENDOR_TOKEN_REGEX.replace_all(&s, REDACTED);
    let s = GOOGLE_API_KEY_REGEX.replace_all(&s, REDACTED);
    let s = BEARER_TOKEN_REGEX.replace_all(&s, format!("Bearer {REDACTED}"));
    let s = JWT_REGEX.replace_all(&s, REDACTED);
    let s = redact_urls_in(&s);
    let s = SECRET_ASSIGNMENT_REGEX
        .replace_all(&s, format!("$1$2$3{REDACTED}"))
        .into_owned();
    Cow::Owned(s)
}

/// Redact secrets in a JSON value, walking all string fields.
pub(crate) fn redact_json_string_values(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            if let Cow::Owned(replaced) = redact_secrets(s) {
                *s = replaced;
            }
        }
        serde_json::Value::Array(arr) => arr.iter_mut().for_each(redact_json_string_values),
        serde_json::Value::Object(map) => map.values_mut().for_each(redact_json_string_values),
        _ => {}
    }
}

/// Reduce a URL to `scheme://host[:port]` — its path/query can carry user
/// content. Unparseable values are returned unchanged.
pub(crate) fn url_origin(value: &str) -> Cow<'_, str> {
    if let Ok(url) = url::Url::parse(value) {
        if let Some(host) = url.host_str() {
            let origin = match url.port() {
                Some(port) => format!("{}://{}:{}", url.scheme(), host, port),
                None => format!("{}://{}", url.scheme(), host),
            };
            return Cow::Owned(origin);
        }
    }
    Cow::Borrowed(value)
}

/// Redact sensitive parts of a URL (credentials, fragment, sensitive query params).
pub(crate) fn redact_url(url: &mut url::Url) {
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);

    let Some(query) = url.query().map(str::to_owned) else {
        return;
    };
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| {
            let key = k.into_owned();
            let value = if SENSITIVE_QUERY_PARAMS
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&key))
            {
                REDACTED_URL_VALUE.to_owned()
            } else {
                v.into_owned()
            };
            (key, value)
        })
        .collect();
    if pairs.is_empty() {
        url.set_query(None);
        return;
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in &pairs {
        serializer.append_pair(k, v);
    }
    url.set_query(Some(&serializer.finish()));
}

/// Collapse `$HOME` to `~` and username path segments to `<user>`.
pub(crate) fn redact_user_paths(input: &str) -> Cow<'_, str> {
    redact_user_paths_with_backstop(input, HOME_DIR.as_deref(), USERNAMES.as_slice())
}

fn redact_user_paths_with_backstop<'a>(
    input: &'a str,
    home: Option<&str>,
    usernames: &[String],
) -> Cow<'a, str> {
    let env_scrubbed = redact_user_paths_env(input, home, usernames);
    if home.is_some() || !usernames.is_empty() {
        return env_scrubbed;
    }
    match HOME_ROOT_USER_REGEX.replace_all(env_scrubbed.as_ref(), "${1}<user>") {
        Cow::Owned(o) => Cow::Owned(o),
        Cow::Borrowed(_) => env_scrubbed,
    }
}

fn redact_user_paths_env<'a>(
    input: &'a str,
    home: Option<&str>,
    usernames: &[String],
) -> Cow<'a, str> {
    let stage1 = match home {
        Some(home) if !home.is_empty() && input.contains(home) => {
            Cow::Owned(replace_home_prefix(input, home))
        }
        _ => Cow::Borrowed(input),
    };
    if !usernames.is_empty() {
        let stage2 = redact_username_segments(stage1.as_ref(), usernames);
        if stage2 != stage1.as_ref() {
            return Cow::Owned(stage2);
        }
    }
    match stage1 {
        Cow::Owned(s) if s != input => Cow::Owned(s),
        _ => Cow::Borrowed(input),
    }
}

fn replace_home_prefix(input: &str, home: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find(home) {
        let (before, tail) = rest.split_at(idx);
        let after = &tail[home.len()..];
        let prev_ok = before.chars().last().is_none_or(is_segment_boundary);
        let next_ok = after.chars().next().is_none_or(is_segment_boundary);
        out.push_str(before);
        if prev_ok && next_ok {
            out.push('~');
        } else {
            out.push_str(home);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

fn redact_username_segments(value: &str, usernames: &[String]) -> String {
    let mut out = String::with_capacity(value.len());
    let mut buf = String::new();
    for ch in value.chars() {
        if is_segment_boundary(ch) {
            push_username_segment(&mut out, &buf, usernames);
            buf.clear();
            out.push(ch);
        } else {
            buf.push(ch);
        }
    }
    push_username_segment(&mut out, &buf, usernames);
    out
}

fn push_username_segment(out: &mut String, segment: &str, usernames: &[String]) {
    let matches = if cfg!(windows) {
        usernames.iter().any(|u| u.eq_ignore_ascii_case(segment))
    } else {
        usernames.iter().any(|u| u == segment)
    };
    out.push_str(if matches { "<user>" } else { segment });
}

fn redact_urls_in(text: &str) -> String {
    URL_REGEX
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let raw = &caps[0];
            url::Url::parse(raw).map_or_else(
                |_| raw.to_owned(),
                |mut url| {
                    redact_url(&mut url);
                    url.to_string()
                },
            )
        })
        .into_owned()
}

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|e| panic!("invalid regex `{pattern}`: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_owned_scrubs_secret_shapes() {
        let out = redact_owned("key sk-CANARYabcdefghij1234567890 end")
            .expect("secret must trigger a rewrite");
        assert!(!out.contains("CANARY"), "secret survived: {out}");
    }

    #[test]
    fn redact_owned_returns_none_when_clean() {
        assert_eq!(redact_owned("no secrets here"), None);
    }

    #[test]
    fn url_origin_drops_path_and_query() {
        let origin = url_origin("https://collector.corp.example:4318/v1/logs?token=CANARY");
        assert_eq!(origin, "https://collector.corp.example:4318");
    }

    #[test]
    fn url_origin_passes_unparseable_through() {
        assert_eq!(url_origin("not a url"), "not a url");
    }

    fn fixture(parts: &[&str]) -> String {
        parts.concat()
    }

    #[test]
    fn redacts_known_secret_shapes() {
        let cases = [
            (
                fixture(&["key: xai-", "abc123XYZdef456GHIjkl789"]),
                "xai api key",
            ),
            (
                fixture(&["aws AKIA", "ABCDEFGHIJKLMNOP key"]),
                "aws access key",
            ),
            (
                fixture(&["Authorization: Bearer eyJhbGciOiJIUzI1NiJ9", ".foo.bar.baz"]),
                "bearer token",
            ),
            (fixture(&["api_key=", "ABCDEFGHIJ"]), "key=value"),
            (
                fixture(&["refresh_token: \"rt_", "abc1234567\""]),
                "compound token name",
            ),
            (
                fixture(&[
                    "deployment key eyJhbGciOiJIUzI1NiJ9",
                    ".eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4f",
                ]),
                "bare jwt without prefix",
            ),
        ];
        for (input, label) in cases {
            let out = redact_secrets(&input);
            assert!(out.contains("[REDACTED_SECRET]"), "{label}: {out:?}");
        }
    }

    #[test]
    fn redacts_additional_provider_prefixes() {
        let cases = [
            (
                fixture(&["token ghp_", "0123456789abcdefghijABCDEFGHIJ012345"]),
                "github classic pat",
            ),
            (
                fixture(&["glpat-", "0123456789abcdefABCD here"]),
                "gitlab pat",
            ),
            (
                fixture(&["xoxb-", "2420837490-2420837490-AbCdEfGhIjKlMnOpQr"]),
                "slack bot token",
            ),
            (
                fixture(&["AIza", "SyD0123456789abcdefghijklmnopqrstuvw"]),
                "google api key",
            ),
        ];
        for (input, label) in cases {
            let out = redact_secrets(&input);
            assert!(out.contains(REDACTED), "{label} not redacted: {out:?}");
        }
    }

    #[test]
    fn does_not_over_redact_sk_lookalikes() {
        for input in [
            "task-deadbeefdeadbeefdeadbeef0123",
            "disk-0123456789abcdefghijklmno",
            "risk-0123456789abcdefghijklmno",
        ] {
            assert_eq!(redact_secrets(input), input, "over-redacted: {input}");
        }
    }

    #[test]
    fn redacts_pem_private_key_block() {
        let input = "key:\n-----BEGIN PRIVATE KEY-----\nMIIabc123def456\nMIIxyz789\n-----END PRIVATE KEY-----\ndone";
        let out = redact_secrets(input);
        assert!(out.contains(REDACTED), "PEM not redacted: {out}");
        assert!(!out.contains("MIIabc123"), "PEM body leaked: {out}");
    }

    #[test]
    fn redacts_sensitive_url_query_params() {
        let out = redact_secrets("callback https://x.ai/cb?code=ABC123XYZ&state=xyz789 failed");
        assert!(!out.contains("ABC123XYZ"), "OAuth code leaked: {out}");
        assert!(!out.contains("xyz789"), "state leaked: {out}");
    }

    #[test]
    fn redact_url_strips_credentials_and_fragment() {
        let mut url = url::Url::parse(
            "https://user:pw@idp.example.com/cb?code=ABC123XYZ&page=2#access_token=DEF456",
        )
        .unwrap();
        redact_url(&mut url);
        let out = url.to_string();
        assert!(!out.contains("user"), "userinfo leaked: {out}");
        assert!(!out.contains("ABC123XYZ"), "OAuth code leaked: {out}");
        assert!(out.contains("page=2"), "lost benign param: {out}");
    }

    #[test]
    fn redact_json_string_values_scrubs_nested_secrets() {
        let mut value = serde_json::json!({
            "key": "sk-abcdefghijklmnopqrstuvwxyz",
            "nested": {
                "token": "ghp_0123456789abcdefghijABCDEFGHIJ012345"
            },
            "safe": "hello"
        });
        redact_json_string_values(&mut value);
        let s = value.to_string();
        assert!(!s.contains("sk-abcdef"), "api key leaked: {s}");
        assert!(!s.contains("ghp_0123"), "github pat leaked: {s}");
        assert!(s.contains("hello"), "safe value lost: {s}");
    }
}
