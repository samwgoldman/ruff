use indexmap::IndexMap;
use ruff_python_stdlib::identifiers::is_identifier;

use crate::docstring::parsing::{
    ParsedLine, indentation, parse_parenthesized_type, parsed_lines, split_once_unbracketed_colon,
};
use crate::docstring::preformatted::PreformattedBlockScanner;
use crate::docstring::sections::SectionKind;

pub(in crate::docstring) struct Docstring {
    parameters: IndexMap<String, String>,
}

impl Docstring {
    pub(in crate::docstring) fn parse(raw: &str) -> Self {
        let parameters = parse_parameter_documentation(raw);
        Self { parameters }
    }

    pub(in crate::docstring) fn parameter_documentation(&self) -> IndexMap<String, String> {
        self.parameters.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoogleSectionKind {
    Supported(SectionKind),
    Unsupported,
}

fn parse_parameter_documentation(raw: &str) -> IndexMap<String, String> {
    let lines = parsed_lines(raw);
    let mut parameters = IndexMap::new();
    let mut preformatted_blocks = PreformattedBlockScanner::default();
    let mut index = 0;

    while index < lines.len() {
        if preformatted_blocks.consume_preformatted_line(lines[index].text) {
            index += 1;
            continue;
        }

        let Some(header) = parse_google_section_like_header(&lines, index) else {
            preformatted_blocks.observe_non_preformatted_line(lines[index].text);
            index += 1;
            continue;
        };
        if header.indent != 0 {
            index += 1;
            continue;
        }

        let body_end = google_section_body_end(&lines, header);
        if matches!(
            header.kind,
            GoogleSectionKind::Supported(SectionKind::Parameters | SectionKind::KeywordArguments)
        ) {
            extend_parameter_documentation(&mut parameters, &lines[header.body_start..body_end]);
        }
        index = body_end;
    }

    parameters
}

fn google_section_body_end(lines: &[ParsedLine], header: GoogleSectionHeader) -> usize {
    let mut body_end = header.body_start;
    let mut body_preformatted_blocks = PreformattedBlockScanner::default();

    while let Some(line) = lines.get(body_end).map(|line| line.text) {
        if body_preformatted_blocks.is_active()
            && body_preformatted_blocks.consume_preformatted_line(line)
        {
            body_end += 1;
            continue;
        }

        if line.trim().is_empty()
            && !google_blank_line_continues_section(&lines[body_end..], header)
        {
            break;
        }

        if google_section_header_ends_body(lines, body_end, header) {
            break;
        }

        if !line.trim().is_empty() && !google_line_belongs_to_body(header, line) {
            break;
        }

        if !body_preformatted_blocks.consume_preformatted_line(line) {
            body_preformatted_blocks.observe_non_preformatted_line(line);
        }
        body_end += 1;
    }

    body_end
}

fn google_blank_line_continues_section(lines: &[ParsedLine], header: GoogleSectionHeader) -> bool {
    let Some((offset, non_blank_line)) = lines
        .iter()
        .enumerate()
        .find(|(_, line)| !line.text.trim().is_empty())
    else {
        return false;
    };

    if google_section_header_ends_body(lines, offset, header) {
        return false;
    }

    google_line_belongs_to_body(header, non_blank_line.text)
}

fn google_section_header_ends_body(
    lines: &[ParsedLine],
    index: usize,
    header: GoogleSectionHeader,
) -> bool {
    let Some(next) = parse_google_section_like_header(lines, index) else {
        return false;
    };

    next.indent <= header.indent
}

fn google_line_belongs_to_body(header: GoogleSectionHeader, line: &str) -> bool {
    indentation(line) > header.indent
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GoogleSectionHeader {
    kind: GoogleSectionKind,
    indent: usize,
    body_start: usize,
}

fn parse_google_section_like_header(
    lines: &[ParsedLine],
    index: usize,
) -> Option<GoogleSectionHeader> {
    let line = lines.get(index)?.text;
    let kind = google_section_kind(line)?;

    Some(GoogleSectionHeader {
        kind,
        indent: indentation(line),
        body_start: index + 1,
    })
}

fn google_section_kind(line: &str) -> Option<GoogleSectionKind> {
    let name = normalized_google_section_name(line)?;
    let kind = match name.as_str() {
        "args" | "arguments" | "parameters" => {
            GoogleSectionKind::Supported(SectionKind::Parameters)
        }
        "keyword args" | "keyword arguments" => {
            GoogleSectionKind::Supported(SectionKind::KeywordArguments)
        }
        "attributes" | "example" | "examples" | "note" | "notes" | "other parameters"
        | "references" | "return" | "returns" | "raise" | "raises" | "see also" | "todo"
        | "todos" | "warning" | "warnings" | "yield" | "yields" => GoogleSectionKind::Unsupported,
        _ => return None,
    };
    Some(kind)
}

fn normalized_google_section_name(line: &str) -> Option<String> {
    let name = line.trim().strip_suffix(':')?.trim();
    Some(
        name.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase(),
    )
}

fn parse_google_parameter(line: &str) -> Option<(String, String)> {
    let (name, description) = split_once_unbracketed_colon(line)?;
    let name = name.trim();
    let (display_name, _) = parse_parenthesized_type(name);
    let lookup_name = google_parameter_lookup_name(display_name)?;

    Some((lookup_name, description.trim().to_string()))
}

fn extend_parameter_documentation(parameters: &mut IndexMap<String, String>, lines: &[ParsedLine]) {
    let mut current: Option<(String, String)> = None;
    let mut item_indent = None;

    for line in lines {
        let line = line.text;
        let trimmed = line.trim();
        let line_indent = indentation(line);

        if trimmed.is_empty() {
            if let Some(current) = &mut current {
                if !current.1.is_empty() && !current.1.ends_with('\n') {
                    current.1.push('\n');
                }
                current.1.push('\n');
            }
            continue;
        }

        if item_indent.is_none_or(|indent| line_indent == indent)
            && let Some(parameter) = parse_google_parameter(trimmed)
        {
            insert_parameter_documentation(parameters, current.replace(parameter));
            item_indent.get_or_insert(line_indent);
            continue;
        }

        if let Some(current) = &mut current {
            if !current.1.is_empty() && !current.1.ends_with('\n') {
                current.1.push('\n');
            }
            current.1.push_str(trimmed);
        }
    }

    insert_parameter_documentation(parameters, current);
}

fn insert_parameter_documentation(
    parameters: &mut IndexMap<String, String>,
    parameter: Option<(String, String)>,
) {
    let Some((name, description)) = parameter else {
        return;
    };
    let description = description.trim().to_string();
    if !description.is_empty() {
        parameters.entry(name).or_insert(description);
    }
}

fn google_parameter_lookup_name(display_name: &str) -> Option<String> {
    let name = display_name.split(',').next()?.trim();
    let identifier = name
        .strip_prefix("**")
        .or_else(|| name.strip_prefix('*'))
        .unwrap_or(name);

    is_identifier(identifier).then(|| name.to_string())
}
