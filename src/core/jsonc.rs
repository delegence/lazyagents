pub fn strip_jsonc(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;

    while let Some(ch) = chars.next() {
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
                output.push(ch);
            }
            continue;
        }

        if in_block_comment {
            if ch == '*' && matches!(chars.peek(), Some('/')) {
                chars.next();
                in_block_comment = false;
            }
            continue;
        }

        if in_string {
            output.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            output.push(ch);
            continue;
        }

        if ch == '/' {
            match chars.peek() {
                Some('/') => {
                    chars.next();
                    in_line_comment = true;
                    continue;
                }
                Some('*') => {
                    chars.next();
                    in_block_comment = true;
                    continue;
                }
                _ => {}
            }
        }

        output.push(ch);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::strip_jsonc;

    #[test]
    fn strips_line_comments() {
        let input = "{\n  // comment\n  \"a\": 1\n}\n";
        let output = strip_jsonc(input);
        assert_eq!(output, "{\n  \n  \"a\": 1\n}\n");
    }

    #[test]
    fn strips_block_comments() {
        let input = "{ /* comment */ \"a\": 1 }";
        let output = strip_jsonc(input);
        assert_eq!(output, "{  \"a\": 1 }");
    }

    #[test]
    fn preserves_strings_with_slashes() {
        let input = "{ \"a\": \"http://example.com/*ok*/\" }";
        let output = strip_jsonc(input);
        assert_eq!(output, input);
    }
}
