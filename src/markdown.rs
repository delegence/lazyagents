use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const ITALIC: &str = "\x1b[3m";
const LIGHT_GRAY: &str = "\x1b[38;5;250m";

/// Formats complete paragraphs and streams code as its lines arrive.
#[derive(Default)]
pub struct StreamBuffer {
    table: bool,
    block: String,
    line: String,
    fence: Option<(u8, usize)>,
    continued_line: bool,
    needs_newline: bool,
}

impl StreamBuffer {
    pub fn push(&mut self, chunk: &str, terminal: bool) -> String {
        let mut output = String::new();
        for character in chunk.chars() {
            self.needs_newline = character != '\n';
            self.line.push(character);
            if character == '\n' {
                let line = self.line.trim();
                let marker = line.as_bytes().first().copied();
                let count = marker.map_or(0, |marker| {
                    line.bytes().take_while(|byte| *byte == marker).count()
                });
                let fence_line =
                    !self.continued_line && matches!(marker, Some(b'`' | b'~')) && count >= 3;
                if let Some((open, length)) = self.fence {
                    output.push_str(&sanitize(&self.line));
                    if fence_line
                        && marker == Some(open)
                        && count >= length
                        && line[count..].trim().is_empty()
                    {
                        self.fence = None;
                    }
                } else if fence_line {
                    self.table = false;
                    output.push_str(&self.flush_block(terminal));
                    output.push_str(&sanitize(&self.line));
                    self.fence = marker.map(|marker| (marker, count));
                } else if terminal && self.table && line.contains('|') {
                    output.push_str(&sanitize(&self.line));
                } else {
                    self.table = false;
                    if terminal {
                        let header = self.block.lines().last().unwrap_or("");
                        let candidate = format!("{header}\n{}", self.line);
                        if Parser::new_ext(&candidate, Options::all())
                            .any(|event| matches!(event, Event::Start(Tag::Table(_))))
                        {
                            let header_start = self
                                .block
                                .trim_end_matches('\n')
                                .rfind('\n')
                                .map_or(0, |index| index + 1);
                            let header = self.block.split_off(header_start);
                            output.push_str(&self.flush_block(terminal));
                            output.push_str(&sanitize(&header));
                            output.push_str(&sanitize(&self.line));
                            self.table = true;
                            self.line.clear();
                            self.continued_line = false;
                            continue;
                        }
                    }
                    let blank = !self.continued_line && line.is_empty();
                    self.block.push_str(&self.line);
                    if blank {
                        output.push_str(&self.flush_block(terminal));
                    }
                }
                self.line.clear();
                self.continued_line = false;
            }
            // Code needs no inline Markdown parsing, so long code lines can stream too.
            if self.fence.is_some() && self.line.len() >= 512 {
                output.push_str(&sanitize(&std::mem::take(&mut self.block)));
                output.push_str(&sanitize(&self.line));
                self.continued_line |= !self.line.is_empty();
                self.line.clear();
            }
        }
        output
    }

    fn flush_block(&mut self, terminal: bool) -> String {
        let text = std::mem::take(&mut self.block);
        if text.is_empty() {
            String::new()
        } else if self.fence.is_some() {
            sanitize(&text)
        } else {
            render(&text, terminal)
        }
    }

    pub fn finish(&mut self, terminal: bool) -> String {
        let mut output = String::new();
        if self.table {
            output.push_str(&sanitize(&std::mem::take(&mut self.line)));
        }
        self.block.push_str(&std::mem::take(&mut self.line));
        output.push_str(&self.flush_block(terminal));
        if self.needs_newline && !output.ends_with('\n') {
            output.push('\n');
        }
        *self = Self::default();
        output
    }
}

pub fn render(source: &str, terminal: bool) -> String {
    let source = sanitize(source);
    if !terminal {
        return with_final_newline(source);
    }

    render_formatted(&source, true)
}

pub fn preview(source: &str) -> String {
    let mut text = String::new();
    for event in Parser::new_ext(&sanitize(source), Options::all()) {
        match event {
            Event::Text(value) | Event::Code(value) => text.push_str(&value),
            Event::SoftBreak
            | Event::HardBreak
            | Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::TableCell
                | TagEnd::TableRow,
            ) => text.push(' '),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn render_plain(source: &str) -> String {
    render_formatted(&sanitize(source), false)
}

fn render_formatted(source: &str, styled: bool) -> String {
    if source.is_empty() {
        return String::new();
    }
    let [bold, italic, light_gray, reset] = if styled {
        [BOLD, ITALIC, LIGHT_GRAY, RESET]
    } else {
        [""; 4]
    };
    let mut output = String::new();
    let mut links = Vec::new();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut table_start = None;
    for (event, range) in Parser::new_ext(source, Options::all()).into_offset_iter() {
        match event {
            Event::Start(Tag::Table(_)) => table_start = Some(range.start),
            Event::End(TagEnd::Table) => {
                if let Some(start) = table_start {
                    let mut rendered = render_formatted(&source[..start], styled);
                    rendered.push_str(&with_final_newline(source[start..range.end].to_owned()));
                    rendered.push_str(&render_formatted(&source[range.end..], styled));
                    return rendered;
                }
            }
            _ => {}
        }
    }
    for event in Parser::new_ext(source, Options::all()) {
        match event {
            Event::Start(Tag::Strong) => output.push_str(bold),
            Event::End(TagEnd::Strong) => output.push_str(reset),
            Event::Start(Tag::Emphasis) => output.push_str(italic),
            Event::End(TagEnd::Emphasis) => output.push_str(reset),
            Event::Start(Tag::Heading { .. }) => output.push_str(bold),
            Event::End(TagEnd::Heading(_)) => {
                output.push_str(reset);
                newline(&mut output);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                output.push_str(light_gray);
                if let CodeBlockKind::Fenced(language) = kind {
                    if !language.is_empty() {
                        output.push_str(language.as_ref());
                        newline(&mut output);
                    }
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                output.push_str(reset);
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
                output.push_str(light_gray);
                output.push_str(&code);
                output.push_str(reset);
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
    fn session_previews_strip_markdown_without_changing_the_message() {
        assert_eq!(preview("### Munich: next 7 days\n\n**Sunny**, with `curl` and [details](https://example.com)."), "Munich: next 7 days Sunny, with curl and details.");
        assert_eq!(
            preview("| Day | Weather |\n|---|---|\n| Thu | **Sunny** |"),
            "Day Weather Thu Sunny"
        );
    }

    #[test]
    fn formats_reasoning_without_terminal_codes_or_markdown_markers() {
        assert_eq!(
            render_plain("\n\n**Checking weather**"),
            "Checking weather\n"
        );
        assert!(render_plain("\n\n").trim().is_empty());
    }

    #[test]
    fn streams_code_lines_before_the_closing_fence() {
        let mut buffer = StreamBuffer::default();
        assert_eq!(
            buffer.push("```rust\nlet x = 1;\n", true),
            "```rust\nlet x = 1;\n"
        );
        assert_eq!(buffer.push("```\n", true), "```\n");
        assert_eq!(buffer.finish(true), "");
    }

    #[test]
    fn preserves_unicode_across_chunks() {
        let source = format!("{}\n\nDone\n", "世界 ".repeat(300));
        let mut buffer = StreamBuffer::default();
        let mut result = String::new();
        for character in source.chars() {
            result.push_str(&buffer.push(&character.to_string(), false));
        }
        result.push_str(&buffer.finish(false));
        assert_eq!(result, source);
    }

    #[test]
    fn keeps_long_paragraph_formatting_across_chunks() {
        let source = format!("**{}** and *done*.\n\n", "世界 ".repeat(300).trim_end());
        let mut buffer = StreamBuffer::default();
        let mut output = String::new();
        for character in source.chars() {
            output.push_str(&buffer.push(&character.to_string(), true));
        }
        output.push_str(&buffer.finish(true));
        assert_eq!(output, render(&source, true));
        assert!(!output.contains("**"));
    }

    #[test]
    fn preserves_table_cells_and_rows() {
        let table = "| Name | Count |\n| --- | --- |\n| Tools | 2 |\n";
        assert_eq!(render(table, true), table);
        assert_eq!(render(table, false), table);
    }

    #[test]
    fn tables_stream_before_completion_and_match_replay() {
        let header = "| Date | Weather |\n| --- | ---: |\n";
        let row = "| Thu | **Sunny** |\n";
        let mut buffer = StreamBuffer::default();
        let mut streamed = String::new();
        for character in header.chars() {
            streamed.push_str(&buffer.push(&character.to_string(), true));
        }
        assert!(streamed.contains("Date"));
        let next = buffer.push(row, true);
        assert!(next.contains("Sunny"));
        assert_eq!(next, row);
        streamed.push_str(&next);
        streamed.push_str(&buffer.finish(true));
        assert_eq!(streamed, render(&format!("{header}{row}"), true));
    }

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
