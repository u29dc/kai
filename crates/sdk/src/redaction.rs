use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value as JsonValue;

static BEARER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(authorization\s*[:=]\s*bearer\s+)([^\s"']+)"#)
        .expect("valid bearer redaction regex")
});
static SECRET_ASSIGNMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(^|[\s,;"'({\[])(\b[A-Z0-9_]*(?:TOKEN|SECRET|API_KEY|AUTH_TOKEN|PASSWORD)\b\s*[:=]\s*)([^\s,;]+)"#,
    )
    .expect("valid secret assignment redaction regex")
});
static URL_SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)([?&](?:token|api[_-]?key|auth[_-]?token|secret)=)([^&\s]+)"#)
        .expect("valid url secret redaction regex")
});
static TELEGRAM_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b\d{8,12}:[A-Za-z0-9_-]{20,}\b"#).expect("valid telegram token regex")
});
static TELEGRAM_BOT_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(/bot)(\d{8,12}:[A-Za-z0-9_-]{20,})"#)
        .expect("valid telegram bot path redaction regex")
});
static GENERIC_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(?:sk|gsk)-[A-Za-z0-9_-]{10,}\b"#).expect("valid generic key regex")
});

const REDACTED: &str = "[REDACTED]";

pub fn redact_text(input: &str) -> String {
    let output = BEARER_RE.replace_all(input, format!("${{1}}{REDACTED}"));
    let output = URL_SECRET_RE.replace_all(&output, format!("${{1}}{REDACTED}"));
    let output = SECRET_ASSIGNMENT_RE.replace_all(&output, format!("${{1}}${{2}}{REDACTED}"));
    let output = TELEGRAM_TOKEN_RE.replace_all(&output, REDACTED);
    let output = TELEGRAM_BOT_PATH_RE.replace_all(&output, format!("${{1}}{REDACTED}"));
    GENERIC_KEY_RE.replace_all(&output, REDACTED).into_owned()
}

pub fn redact_optional_text(input: Option<&str>) -> Option<String> {
    input.map(redact_text)
}

pub fn redact_json_value(value: &mut JsonValue) {
    match value {
        JsonValue::String(text) => {
            *text = redact_text(text);
        }
        JsonValue::Array(items) => {
            for item in items {
                redact_json_value(item);
            }
        }
        JsonValue::Object(map) => {
            for item in map.values_mut() {
                redact_json_value(item);
            }
        }
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_text_masks_bearer_and_assignment_secrets() {
        let input = "Authorization: Bearer abc123 SECRET=super GROQ_API_KEY=gsk_secret";
        let output = redact_text(input);
        assert!(output.contains("Authorization: Bearer [REDACTED]"));
        assert!(output.contains("SECRET=[REDACTED]"));
        assert!(output.contains("GROQ_API_KEY=[REDACTED]"));
    }

    #[test]
    fn redact_text_masks_url_and_telegram_tokens() {
        let input =
            "https://x.test?token=abc&api_key=def [REDACTED-TELEGRAM-TOKEN]";
        let output = redact_text(input);
        assert!(output.contains("?token=[REDACTED]"));
        assert!(output.contains("&api_key=[REDACTED]"));
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("8581338097:AAF"));
    }

    #[test]
    fn redact_text_masks_telegram_bot_url_paths() {
        let input =
            "https://api.telegram.org/bot[REDACTED-TELEGRAM-TOKEN]/getUpdates";
        let output = redact_text(input);
        assert!(output.contains("/bot[REDACTED]/getUpdates"));
        assert!(!output.contains("8581338097:AAF"));
    }
}
