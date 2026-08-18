//! Transcripts contain live credentials. Nothing leaves this machine unscrubbed.

use std::sync::OnceLock;

use regex::{Captures, Regex};

/// Ordered: the specific shapes first, so a key is labelled for what it is
/// before the catch-all entropy rules can swallow it.
fn rules() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        let r = |p: &str| Regex::new(p).expect("redaction pattern");
        vec![
            (
                r(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----"),
                "[REDACTED:private-key]",
            ),
            (r(r"sk-ant-[A-Za-z0-9_-]{16,}"), "[REDACTED:anthropic-key]"),
            (r(r"sk-[A-Za-z0-9_-]{16,}"), "[REDACTED:openai-key]"),
            (r(r"github_pat_[A-Za-z0-9_]{20,}"), "[REDACTED:github-token]"),
            (r(r"gh[pousr]_[A-Za-z0-9]{20,}"), "[REDACTED:github-token]"),
            (r(r"AKIA[0-9A-Z]{16}"), "[REDACTED:aws-key]"),
            (r(r"xox[baprs]-[A-Za-z0-9-]{10,}"), "[REDACTED:slack-token]"),
            (
                r(r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"),
                "[REDACTED:jwt]",
            ),
            (
                r(r"Authorization:\s*(?:Bearer|Basic)\s+\S+"),
                "Authorization: [REDACTED:auth-header]",
            ),
            (
                // The value must not start with `[`, so a marker an earlier rule
                // already wrote keeps the label that says what was removed.
                r(r#"(?i)(api[_-]?key|secret|token|password|passwd)\s*[:=]\s*["']?[^\s"',}\[][^\s"',}]{11,}"#),
                "${1}=[REDACTED:secret]",
            ),
        ]
    })
}

/// A 40-char hex run is as likely to be a git SHA as a key. Both are redacted:
/// losing a commit hash from a summary costs a little context, leaking a key
/// costs a lot. Same for the base64-ish runs, which on this machine are almost
/// all opaque blobs (encrypted reasoning, content hashes) that read as noise
/// anyway. Neither class contains `/`, `-` or `_`, so file paths, project dir
/// names like `-Volumes-T9-agent-monitor` and UUIDs survive intact.
fn entropy_rules() -> &'static [Regex] {
    static RULES: OnceLock<Vec<Regex>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            Regex::new(r"\b[0-9a-fA-F]{40,}\b").unwrap(),
            Regex::new(r"[A-Za-z0-9+]{40,}={0,2}").unwrap(),
        ]
    })
}

pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for (re, marker) in rules() {
        if re.is_match(&out) {
            out = re.replace_all(&out, *marker).into_owned();
        }
    }
    for re in entropy_rules() {
        out = re
            .replace_all(&out, |c: &Captures| {
                let run = &c[0];
                // Letters only is a word; digits only is a number. Neither is a secret.
                let mixed = run.bytes().any(|b| b.is_ascii_digit())
                    && run.bytes().any(|b| b.is_ascii_alphabetic());
                if mixed {
                    "[REDACTED:high-entropy]".to_string()
                } else {
                    run.to_string()
                }
            })
            .into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    // Every credential-shaped string below is a literal `EXAMPLE` filler that
    // satisfies the pattern under test. None of them is, or ever was, a real key.
    use super::redact;

    fn scrub(input: &str, expect_marker: &str) -> String {
        let out = redact(input);
        assert!(out.contains(expect_marker), "{input} -> {out}");
        out
    }

    #[test]
    fn openai_style_key() {
        let out = scrub(
            "我需要他用DeepSeek的API：sk-EXAMPLEEXAMPLEEXAMPLE00000000 谢谢",
            "[REDACTED:openai-key]",
        );
        assert!(!out.contains("sk-EXAMPLE"));
        assert!(out.contains("我需要他用DeepSeek的API：") && out.contains("谢谢"));
    }

    #[test]
    fn anthropic_key_is_labelled_as_such() {
        let out = scrub(
            "export ANTHROPIC_API_KEY=sk-ant-api03-EXAMPLEEXAMPLEEXAMPLEEXAMPLE",
            "[REDACTED:anthropic-key]",
        );
        assert!(!out.contains("api03"));
    }

    #[test]
    fn github_tokens() {
        scrub(
            "token ghp_EXAMPLEEXAMPLEEXAMPLE0000000000",
            "[REDACTED:github-token]",
        );
        scrub(
            "gho_EXAMPLEEXAMPLEEXAMPLE00000 and ghs_EXAMPLEEXAMPLEEXAMPLE11111",
            "[REDACTED:github-token]",
        );
        scrub(
            "github_pat_11EXAMPLE0EXAMPLE_EXAMPLEEXAMPLEEXAMPLE",
            "[REDACTED:github-token]",
        );
    }

    #[test]
    fn aws_and_slack() {
        scrub("aws id AKIAIOSFODNN7EXAMPLE here", "[REDACTED:aws-key]");
        scrub(
            "xoxb-EXAMPLE-EXAMPLE-EXAMPLEEXAMPLEEXAMPLE",
            "[REDACTED:slack-token]",
        );
    }

    #[test]
    fn jwt() {
        let out = scrub(
            "cookie=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJFWEFNUExFIn0.EXAMPLE-EXAMPLE-EXAMPLE-EXAMPLE",
            "[REDACTED:jwt]",
        );
        assert!(!out.contains("eyJhbGciOi"));
    }

    #[test]
    fn private_key_block() {
        let out = scrub(
            "before\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nAAAABG5vbmUAAAAEbm9uZQAA\n-----END OPENSSH PRIVATE KEY-----\nafter",
            "[REDACTED:private-key]",
        );
        assert!(out.starts_with("before\n") && out.ends_with("\nafter"));
        assert!(!out.contains("b3BlbnNzaC1rZXktdjEAAAAA"));
    }

    #[test]
    fn authorization_header() {
        let out = scrub(
            "curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz' https://api.example.com",
            "[REDACTED:auth-header]",
        );
        assert!(!out.contains("abcdefghijklmnop"));
        assert!(out.contains("https://api.example.com"));
        scrub(
            "Authorization:   Basic dXNlcjpwYXNzd29yZA==",
            "[REDACTED:auth-header]",
        );
    }

    #[test]
    fn key_value_assignments() {
        for line in [
            "api_key = 'hunter2hunter2hunter2'",
            "API-KEY: abcdefghijklmnop",
            "password: correct-horse-battery",
            "PASSWD=correct-horse-battery",
            "client_secret=\"s3cr3ts3cr3ts3cr3t\"",
            "refresh_token: 0123456789abcdefghij",
        ] {
            let out = scrub(line, "[REDACTED:secret]");
            assert!(!out.contains("hunter2") && !out.contains("horse-battery"), "{out}");
        }
    }

    #[test]
    fn high_entropy_runs() {
        // A 64-char hex digest and a base64 blob, neither of which announces itself.
        scrub(
            "digest 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08 ok",
            "[REDACTED:high-entropy]",
        );
        scrub(
            "blob Qk3zLpVt7WmXeR2aYdHn5CfUj8SgKbNo1ZiTvQxMwEr4PyLuAsDgFhJkZcVbNm6Q",
            "[REDACTED:high-entropy]",
        );
    }

    #[test]
    fn a_git_sha_is_redacted_on_purpose() {
        // 40 hex chars is both a commit hash and a plausible key. We cannot tell
        // them apart, and a mangled summary is cheaper than a leaked credential.
        let out = redact("fixed in 0e2a3174d5f40de49ed383507497d69bbff42c5d, see PR #58");
        assert!(out.contains("[REDACTED:high-entropy]"));
        assert!(out.contains("fixed in ") && out.contains(", see PR #58"));
    }

    #[test]
    fn ordinary_text_survives() {
        for line in [
            "The daemon reads ~/.claude/projects and never writes to it.",
            "/repo/agent-monitor/daemon/src/adapters/claude.rs:118",
            "/home/u/.claude/projects/-home-u-agent-monitor-daemon/session.jsonl",
            "session 019fc618-9593-7922-bd46-b3f7e31f160e ended after 12 turns",
            "cargo test redact -- --nocapture # 3 tokens used, max_tokens: 32000",
            "Supercalifragilisticexpialidocious antidisestablishmentarianism pneumonoultramicroscopic",
        ] {
            assert_eq!(redact(line), line, "false positive");
        }
    }
}
