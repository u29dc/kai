use super::{
    MobileCommand, TELEGRAM_TEXT_LIMIT, failure_notice_text, format_telegram_html,
    parse_mobile_command, should_retry_telegram_send, should_skip_failed_update,
    split_response_text, stable_pending_turn_id,
};
use crate::error::{ErrorCode, KaiError};

#[test]
fn format_telegram_html_renders_inline_code_and_bold() {
    let input = "Use `rg` and **be precise**.";
    let output = format_telegram_html(input);
    assert_eq!(output, "Use <code>rg</code> and <b>be precise</b>.");
}

#[test]
fn format_telegram_html_renders_fenced_code_block() {
    let input = "Example:\n```rust\nlet x = 1 < 2;\n```\nDone.";
    let output = format_telegram_html(input);
    assert_eq!(
        output,
        "Example:\n\n<pre>rust\nlet x = 1 &lt; 2;</pre>\n\nDone."
    );
}

#[test]
fn format_telegram_html_escapes_raw_html() {
    let input = "<b>unsafe</b> `ok`";
    let output = format_telegram_html(input);
    assert_eq!(output, "&lt;b&gt;unsafe&lt;/b&gt; <code>ok</code>");
}

#[test]
fn format_telegram_html_renders_lists_and_links() {
    let input = "- one\n- two\n\n[site](https://example.com)";
    let output = format_telegram_html(input);
    assert_eq!(
        output,
        "• one\n• two\n\n<a href=\"https://example.com\">site</a>"
    );
}

#[test]
fn split_response_text_splits_long_messages() {
    let input = "a".repeat(TELEGRAM_TEXT_LIMIT + 20);
    let chunks = split_response_text(&input);
    assert_eq!(chunks.len(), 2);
    assert!(
        chunks
            .iter()
            .all(|chunk| format_telegram_html(chunk).chars().count() <= TELEGRAM_TEXT_LIMIT)
    );
}

#[test]
fn split_response_text_balances_fenced_code_blocks() {
    let input = format!(
        "{}\n```rust\n{}\n```",
        "a".repeat(TELEGRAM_TEXT_LIMIT - 20),
        "let x = 1;"
    );
    let chunks = split_response_text(&input);
    assert!(chunks.len() >= 2);
    for chunk in chunks {
        let fences = chunk
            .lines()
            .filter(|line| line.trim_start().starts_with("```"))
            .count();
        assert_eq!(fences % 2, 0);
    }
}

#[test]
fn split_response_text_respects_rendered_html_limit() {
    let input = "<".repeat(TELEGRAM_TEXT_LIMIT / 2);
    let chunks = split_response_text(&input);
    assert!(chunks.len() >= 2);
    assert!(
        chunks
            .iter()
            .all(|chunk| format_telegram_html(chunk).chars().count() <= TELEGRAM_TEXT_LIMIT)
    );
}

#[test]
fn stable_pending_turn_id_is_deterministic_and_order_sensitive() {
    let first = stable_pending_turn_id("telegram", 1, 2, &[100, 101, 102]);
    let second = stable_pending_turn_id("telegram", 1, 2, &[100, 101, 102]);
    let reordered = stable_pending_turn_id("telegram", 1, 2, &[102, 101, 100]);

    assert_eq!(first, second);
    assert_ne!(first, reordered);
}

#[test]
fn parse_mobile_command_recognizes_side_query_and_cancel_alias() {
    match parse_mobile_command("/ask check current Rust release notes") {
        Some(MobileCommand::Ask { prompt }) => {
            assert_eq!(prompt, "check current Rust release notes");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    assert!(matches!(
        parse_mobile_command("/cancel ask"),
        Some(MobileCommand::Cancel { side_query: true })
    ));
}

#[test]
fn invalid_argument_updates_are_skipped_immediately() {
    let error = KaiError::invalid_argument("too many attachments");
    assert!(should_skip_failed_update(&error, 1));
    assert_eq!(
        failure_notice_text(&error, 1),
        "I couldn't handle that message: too many attachments"
    );
}

#[test]
fn retryable_errors_are_skipped_after_threshold() {
    let error = KaiError::new(ErrorCode::RuntimeError, "temporary backend issue");
    assert!(!should_skip_failed_update(&error, 1));
    assert!(should_skip_failed_update(&error, 3));
}

#[test]
fn telegram_send_retry_classifier_matches_common_retryable_errors() {
    let error = KaiError::new(
        ErrorCode::RuntimeError,
        "Telegram API returned 429 Too Many Requests",
    );
    assert!(should_retry_telegram_send(&error));
    let error = KaiError::new(
        ErrorCode::RuntimeError,
        "failed to send Telegram message: connection reset by peer",
    );
    assert!(should_retry_telegram_send(&error));
    let error = KaiError::new(ErrorCode::RuntimeError, "bot was blocked by the user");
    assert!(!should_retry_telegram_send(&error));
}
