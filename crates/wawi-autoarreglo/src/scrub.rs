//! Secrets out of what leaves, and out of what is proposed. One list of
//! patterns, two directions (as in Wawi 1):
//!
//! - [`scrub`] before anything goes into a brief, a proposal or the journal:
//!   tokens, keys, e-mail addresses, LAN addresses and home paths become
//!   placeholders.
//! - [`scan_added`] over the `+` lines of a candidate diff: anything that
//!   looks like a credential or a personal path rejects the attempt.

use std::sync::OnceLock;

use regex::Regex;

struct Patterns {
    secrets: Vec<(&'static str, Regex)>,
    email: Regex,
    any_home: Regex,
    lan_ip: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let re = |p: &str| Regex::new(p).expect("a fixed pattern");
        Patterns {
            secrets: vec![
                ("private_key", re(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
                ("anthropic_key", re(r"sk-ant-[A-Za-z0-9_-]{16,}")),
                ("openai_key", re(r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}")),
                (
                    "github_token",
                    re(r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})"),
                ),
                ("linear_key", re(r"\blin_(?:api|oauth)_[A-Za-z0-9]{20,}")),
                ("slack_token", re(r"\bxox[abprs]-[A-Za-z0-9-]{10,}")),
                ("aws_key", re(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b")),
                ("google_key", re(r"\bAIza[0-9A-Za-z_-]{35}\b")),
                ("stripe_key", re(r"\b(?:sk|rk)_(?:live|test)_[0-9A-Za-z]{16,}")),
                (
                    "jwt",
                    re(r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"),
                ),
                ("bearer", re(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{20,}")),
                (
                    "assigned_secret",
                    re(
                        r#"(?i)\b(?:api[_-]?key|secret|token|passw(?:or)?d|auth)\b\s*[:=]\s*['"][^'"\s]{12,}['"]"#,
                    ),
                ),
                ("url_credentials", re(r"\b[a-z][a-z0-9+.-]*://[^/\s:@]+:[^/\s@]+@")),
            ],
            email: re(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b"),
            any_home: re(r"/Users/[A-Za-z0-9._-]+|/home/[A-Za-z0-9._-]+"),
            lan_ip: re(
                r"\b(?:192\.168|10\.\d{1,3}|172\.(?:1[6-9]|2\d|3[01]))\.\d{1,3}\.\d{1,3}\b",
            ),
        }
    })
}

/// Addresses that are fine in code (examples, attribution trailers).
const EMAIL_ALLOW: &[&str] = &[
    "noreply@anthropic.com",
    "@example.com",
    "@example.org",
    "@users.noreply.github.com",
    "autoarreglo@wawi.local",
];

/// `$HOME`, for the home-path placeholder.
fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// `text` with credentials, e-mail addresses, LAN addresses and home paths
/// replaced by placeholders.
pub fn scrub(text: &str) -> String {
    scrub_with(text, &home_dir())
}

pub fn scrub_with(text: &str, home: &str) -> String {
    let p = patterns();
    let mut out = text.to_owned();
    for (name, pattern) in &p.secrets {
        out = pattern.replace_all(&out, format!("<{name}>")).into_owned();
    }
    out = p.email.replace_all(&out, "<email>").into_owned();
    if home.len() > 1 {
        out = out.replace(home, "~");
    }
    out = p.any_home.replace_all(&out, "~").into_owned();
    p.lan_ip.replace_all(&out, "<lan-ip>").into_owned()
}

/// `(file, line)` for every added line of a unified diff.
pub fn added_lines(diff: &str) -> Vec<(String, String)> {
    let mut current = String::new();
    let mut rows = Vec::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            current = path.trim().trim_start_matches("b/").to_owned();
            continue;
        }
        if let Some(added) = line.strip_prefix('+') {
            rows.push((current.clone(), added.to_owned()));
        }
    }
    rows
}

/// What looks secret or personal in the added lines. Empty = clean.
pub fn scan_added(diff: &str, home: &str, user: &str) -> Vec<String> {
    let p = patterns();
    let mut found: Vec<String> = Vec::new();
    let user_re = (user.len() >= 4)
        .then(|| Regex::new(&format!(r"\b{}\b", regex::escape(user))).ok())
        .flatten();
    for (path, line) in added_lines(diff) {
        let mut add = |what: &str| {
            let item = format!("{path}: {what}");
            if !found.contains(&item) {
                found.push(item);
            }
        };
        for (name, pattern) in &p.secrets {
            if pattern.is_match(&line) {
                add(name);
            }
        }
        for email in p.email.find_iter(&line) {
            let email = email.as_str();
            if !EMAIL_ALLOW
                .iter()
                .any(|a| email == *a || email.ends_with(a))
            {
                add("email");
            }
        }
        if (home.len() > 1 && line.contains(home)) || p.any_home.is_match(&line) {
            add("ruta personal");
        } else if user_re.as_ref().is_some_and(|re| re.is_match(&line)) {
            add("nombre de usuario");
        }
        if p.lan_ip.is_match(&line) {
            add("IP de la red local");
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_keys_mails_homes_and_lan() {
        let text = "key sk-ant-abcdefghijklmnopqrstuv mail ana@gmail.com en /Users/ana/x y 192.168.1.20, Bearer abcdefghijklmnopqrstuvwxyz";
        let out = scrub_with(text, "/Users/ana");
        assert!(!out.contains("sk-ant-abc"), "{out}");
        assert!(!out.contains("ana@gmail.com"), "{out}");
        assert!(!out.contains("/Users/ana"), "{out}");
        assert!(!out.contains("192.168.1.20"), "{out}");
        assert!(!out.contains("abcdefghijklmnopqrstuvwxyz"), "{out}");
        assert!(out.contains("~/x"), "{out}");
    }

    #[test]
    fn scans_only_added_lines() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-let old = \"ghp_abcdefghijklmnopqrstuvwxyz0123456789\";\n+let path = \"/Users/ana/secretos\";\n+let token = \"lin_api_abcdefghijklmnopqrstuvwxyz\";\n+// Co-Authored-By: x <noreply@anthropic.com>\n";
        let found = scan_added(diff, "/Users/ana", "ana");
        assert!(
            found.contains(&"src/a.rs: ruta personal".to_owned()),
            "{found:?}"
        );
        assert!(
            found.contains(&"src/a.rs: linear_key".to_owned()),
            "{found:?}"
        );
        assert!(!found.iter().any(|f| f.contains("github")), "{found:?}");
        assert!(!found.iter().any(|f| f.contains("email")), "{found:?}");
        assert!(scan_added("+++ b/x\n+let x = 1;\n", "/Users/ana", "ana").is_empty());
    }
}
