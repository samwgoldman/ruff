use ruff_python_trivia::leading_indentation;
use ruff_source_file::UniversalNewlines;
use ruff_text_size::TextRange;

/// Calculate indentation width, treating tabs like Python does.
pub(super) fn indentation(line: &str) -> usize {
    leading_indentation(line)
        .chars()
        .map(|char| if char == '\t' { 8 } else { 1 })
        .sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::docstring) struct ParsedLine<'a> {
    pub(in crate::docstring) text: &'a str,
    pub(in crate::docstring) range: TextRange,
}

pub(in crate::docstring) fn parsed_lines(raw: &str) -> Vec<ParsedLine<'_>> {
    raw.universal_newlines()
        .map(|line| ParsedLine {
            text: line.as_str(),
            range: line.range(),
        })
        .collect()
}

pub(in crate::docstring) fn split_once_unbracketed_colon(line: &str) -> Option<(&str, &str)> {
    let mut parentheses = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut quote = None;
    let mut escaped = false;

    for (index, char) in line.char_indices() {
        if let Some(quote_char) = quote {
            if escaped {
                escaped = false;
            } else if char == '\\' {
                escaped = true;
            } else if char == quote_char {
                quote = None;
            }
            continue;
        }

        match char {
            '\'' | '"' => quote = Some(char),
            '(' => parentheses += 1,
            ')' => parentheses = parentheses.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            ':' if parentheses == 0 && brackets == 0 && braces == 0 => {
                return Some((&line[..index], &line[index + ':'.len_utf8()..]));
            }
            _ => {}
        }
    }

    None
}

pub(in crate::docstring) fn parse_parenthesized_type(name: &str) -> (&str, Option<&str>) {
    if !name.ends_with(')') {
        return (name, None);
    }

    let mut depth = 0usize;
    for (index, char) in name.char_indices().rev() {
        match char {
            ')' => depth += 1,
            '(' => {
                if depth == 0 {
                    return (name, None);
                }
                depth -= 1;
                if depth == 0 {
                    let display_name = name[..index].trim();
                    let ty = name[index + '('.len_utf8()..name.len() - ')'.len_utf8()].trim();
                    return if display_name.is_empty() || ty.is_empty() {
                        (name, None)
                    } else {
                        (display_name, Some(ty))
                    };
                }
            }
            _ => {}
        }
    }

    (name, None)
}

pub(in crate::docstring) fn is_docstring_type_expression(ty: &str) -> bool {
    if ty.is_empty() || !ty.chars().all(is_docstring_type_expression_char) {
        return false;
    }

    if !ty.chars().any(char::is_whitespace) {
        return true;
    }

    ty.contains('[') || ty.contains(',') || ty.contains('|') || ty.contains('`')
}

fn is_docstring_type_expression_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || "_.[](){},|\"':/ `~-".contains(ch)
}
