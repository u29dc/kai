use super::*;

pub(super) fn split_response_text(text: &str) -> Vec<String> {
    if telegram_rendered_len(text) <= TELEGRAM_TEXT_LIMIT {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text.to_string();

    while !remaining.is_empty() {
        if telegram_rendered_len(&remaining) <= TELEGRAM_TEXT_LIMIT {
            chunks.push(remaining);
            break;
        }

        let split_at = find_split_index(&remaining);

        let mut chunk = remaining[..split_at].to_string();
        let mut next_remaining = remaining[split_at..].to_string();
        if next_remaining.starts_with('\n') {
            next_remaining = next_remaining[1..].to_string();
        }

        let fence_count = chunk
            .lines()
            .filter(|line| line.trim_start().starts_with("```"))
            .count();
        if fence_count % 2 == 1 {
            chunk.push('\n');
            chunk.push_str("```");
            if !next_remaining.is_empty() {
                next_remaining = format!("```\n{next_remaining}");
            }
        }

        chunks.push(chunk);
        remaining = next_remaining;
    }

    chunks
}

fn find_split_index(remaining: &str) -> usize {
    let mut best_newline = 0_usize;
    let mut best_end = 0_usize;
    for (index, ch) in remaining.char_indices() {
        let end = index + ch.len_utf8();
        if telegram_rendered_len(&remaining[..end]) > TELEGRAM_TEXT_LIMIT {
            break;
        }
        best_end = end;
        if ch == '\n' && index > 0 {
            best_newline = index;
        }
    }
    if best_newline > 0 {
        best_newline
    } else if best_end > 0 {
        best_end
    } else {
        floor_char_boundary(remaining, TELEGRAM_TEXT_LIMIT.min(remaining.len()))
    }
}

fn telegram_rendered_len(text: &str) -> usize {
    format_telegram_html(text).chars().count()
}

fn floor_char_boundary(input: &str, mut index: usize) -> usize {
    while index > 0 && !input.is_char_boundary(index) {
        index -= 1;
    }
    index
}

pub(super) fn format_telegram_html(input: &str) -> String {
    let mut output = String::new();
    let mut list_stack = Vec::new();
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_TABLES);

    for event in Parser::new_ext(input, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { .. } => output.push_str("<b>"),
                Tag::BlockQuote(_) => output.push_str("<blockquote>"),
                Tag::CodeBlock(kind) => {
                    if !output.ends_with('\n') && !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str("<pre>");
                    if let CodeBlockKind::Fenced(language) = kind
                        && !language.trim().is_empty()
                    {
                        output.push_str(&escape_html(language.as_ref()));
                        output.push('\n');
                    }
                }
                Tag::Strong => output.push_str("<b>"),
                Tag::Emphasis => output.push_str("<i>"),
                Tag::Strikethrough => output.push_str("<s>"),
                Tag::Link { dest_url, .. } => {
                    output.push_str("<a href=\"");
                    output.push_str(&escape_html_attr(dest_url.as_ref()));
                    output.push_str("\">");
                }
                Tag::List(start) => list_stack.push(ListKind::new(start)),
                Tag::Item => {
                    if !output.ends_with('\n') && !output.is_empty() {
                        output.push('\n');
                    }
                    let prefix = match list_stack.last_mut() {
                        Some(ListKind::Ordered(next)) => {
                            let current = *next;
                            *next += 1;
                            format!("{current}. ")
                        }
                        _ => "• ".to_string(),
                    };
                    output.push_str(&prefix);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => push_block_break(&mut output),
                TagEnd::Heading(_) => {
                    output.push_str("</b>");
                    push_block_break(&mut output);
                }
                TagEnd::BlockQuote(_) => {
                    output.push_str("</blockquote>");
                    push_block_break(&mut output);
                }
                TagEnd::CodeBlock => {
                    while output.ends_with('\n') {
                        output.pop();
                    }
                    output.push_str("</pre>");
                    push_block_break(&mut output);
                }
                TagEnd::Strong => output.push_str("</b>"),
                TagEnd::Emphasis => output.push_str("</i>"),
                TagEnd::Strikethrough => output.push_str("</s>"),
                TagEnd::Link => output.push_str("</a>"),
                TagEnd::List(_) => {
                    list_stack.pop();
                    push_block_break(&mut output);
                }
                TagEnd::Item => {}
                _ => {}
            },
            Event::Text(text) | Event::InlineHtml(text) | Event::Html(text) => {
                output.push_str(&escape_html(text.as_ref()));
            }
            Event::Code(text) => {
                output.push_str("<code>");
                output.push_str(&escape_html(text.as_ref()));
                output.push_str("</code>");
            }
            Event::SoftBreak | Event::HardBreak => output.push('\n'),
            Event::Rule => {
                if !output.ends_with('\n') && !output.is_empty() {
                    output.push('\n');
                }
                output.push_str("────────");
                push_block_break(&mut output);
            }
            Event::TaskListMarker(checked) => {
                output.push_str(if checked { "[x] " } else { "[ ] " });
            }
            Event::FootnoteReference(name) => {
                output.push('[');
                output.push_str(&escape_html(name.as_ref()));
                output.push(']');
            }
            _ => {}
        }
    }

    output.trim_end().to_string()
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_html_attr(input: &str) -> String {
    escape_html(input).replace('"', "&quot;")
}

fn push_block_break(output: &mut String) {
    while output.ends_with('\n') {
        output.pop();
    }
    if !output.is_empty() {
        output.push('\n');
        output.push('\n');
    }
}
