use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const ITALIC: &str = "\x1b[3m";
const LIGHT_GRAY: &str = "\x1b[38;5;250m";

pub fn render(source: &str, terminal: bool) -> String {
    let source = sanitize(source);
    if !terminal {
        return with_final_newline(source);
    }

    let mut output = String::new();
    let mut links = Vec::new();
    let mut lists: Vec<Option<u64>> = Vec::new();
    for event in Parser::new_ext(&source, Options::all()) {
        match event {
            Event::Start(Tag::Strong) => output.push_str(BOLD),
            Event::End(TagEnd::Strong) => output.push_str(RESET),
            Event::Start(Tag::Emphasis) => output.push_str(ITALIC),
            Event::End(TagEnd::Emphasis) => output.push_str(RESET),
            Event::Start(Tag::Heading { .. }) => output.push_str(BOLD),
            Event::End(TagEnd::Heading(_)) => {
                output.push_str(RESET);
                newline(&mut output);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                output.push_str(LIGHT_GRAY);
                if let CodeBlockKind::Fenced(language) = kind {
                    if !language.is_empty() {
                        output.push_str(language.as_ref());
                        newline(&mut output);
                    }
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                output.push_str(RESET);
                newline(&mut output);
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                output.push('[');
                links.push(dest_url.into_string());
            }
            Event::End(TagEnd::Link) => {
                output.push_str("](");
                output.push_str(&links.pop().unwrap_or_default());
                output.push(')');
            }
            Event::Start(Tag::List(start)) => lists.push(start),
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                newline(&mut output);
            }
            Event::Start(Tag::Item) => {
                newline(&mut output);
                output.push_str(&"  ".repeat(lists.len().saturating_sub(1)));
                match lists.last_mut() {
                    Some(Some(number)) => {
                        output.push_str(&format!("{number}. "));
                        *number += 1;
                    }
                    _ => output.push_str("- "),
                }
            }
            Event::End(TagEnd::Item) => newline(&mut output),
            Event::End(TagEnd::Paragraph) => newline(&mut output),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                output.push_str(&text)
            }
            Event::Code(code) => {
                output.push_str(LIGHT_GRAY);
                output.push_str(&code);
                output.push_str(RESET);
            }
            Event::SoftBreak | Event::HardBreak => newline(&mut output),
            Event::Rule => {
                newline(&mut output);
                output.push_str("------------------------------------");
                newline(&mut output);
            }
            Event::TaskListMarker(checked) => {
                output.push_str(if checked { "[x] " } else { "[ ] " });
            }
            _ => {}
        }
    }
    with_final_newline(output)
}

pub fn sanitize(source: &str) -> String {
    source
        .chars()
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect()
}

fn newline(output: &mut String) {
    if !output.ends_with('\n') {
        output.push('\n');
    }
}

fn with_final_newline(mut output: String) -> String {
    newline(&mut output);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_common_terminal_markdown() {
        let rendered = render("**Bold** and *italic* with `code`", true);
        assert!(rendered.contains("\x1b[1mBold\x1b[0m"));
        assert!(rendered.contains("\x1b[3mitalic\x1b[0m"));
        assert!(rendered.contains("\x1b[38;5;250mcode\x1b[0m"));
    }

    #[test]
    fn keeps_lists_and_clickable_link_text() {
        let rendered = render("* one\n* [two](https://example.com)", true);
        assert!(rendered.contains("- one"));
        assert!(rendered.contains("- [two](https://example.com)"));
    }

    #[test]
    fn keeps_ordered_list_numbers() {
        let rendered = render("3. three\n4. four", true);
        assert!(rendered.contains("3. three"));
        assert!(rendered.contains("4. four"));
    }

    #[test]
    fn strips_terminal_control_characters() {
        assert_eq!(
            sanitize("safe\x1b]52;clipboard\x07\rtext"),
            "safe]52;clipboardtext"
        );
        assert_eq!(sanitize("one\ntwo\tthree"), "one\ntwo\tthree");
    }
}
