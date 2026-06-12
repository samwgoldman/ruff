use ruff_python_stdlib::identifiers::is_identifier;
use ruff_text_size::TextRange;

use crate::docstring::formats;
use crate::docstring::parsing::{
    ParsedLine, is_docstring_type_expression, parse_parenthesized_type,
    split_once_unbracketed_colon,
};
use crate::docstring::preformatted::PreformattedBlockScanner;
use crate::docstring::sections::{Section, SectionItem, SectionKind};

use super::{
    DescriptionLine, SectionItemBuilder, SectionSource, is_uri_scheme_prefix,
    normalize_description, parse_named_items,
};

impl SectionSource for formats::google::Docstring<'_> {
    fn structured_sections(&self) -> Vec<Section> {
        self.sections()
            .iter()
            .filter_map(|section| {
                let kind = section.kind();
                google_section_block(kind, section.range(), section.body())
            })
            .collect()
    }
}

fn google_section_block(
    kind: SectionKind,
    range: TextRange,
    body: &[ParsedLine<'_>],
) -> Option<Section> {
    let items = match kind {
        SectionKind::Returns | SectionKind::Yields => parse_google_return_item(kind, body),
        _ => parse_named_items(kind, body, |line| parse_google_named_item(kind, line)),
    }?;

    Section::new(range, items)
}

fn parse_google_named_item(kind: SectionKind, line: &str) -> Option<SectionItemBuilder> {
    if formats::google::is_section_like_header(line) {
        return None;
    }

    let (name, description) = split_once_google_field_colon(line)?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    let (display_name, ty) = match kind {
        SectionKind::Parameters | SectionKind::KeywordArguments | SectionKind::OtherParameters => {
            let (display_name, ty) = parse_parenthesized_type(name);
            if !is_google_parameter_display_name(display_name) {
                return None;
            }
            (display_name.to_string(), ty.map(str::to_string))
        }
        SectionKind::Attributes => {
            let (display_name, ty) = parse_parenthesized_type(name);
            if !is_google_attribute_display_name(display_name) {
                return None;
            }
            (display_name.to_string(), ty.map(str::to_string))
        }
        SectionKind::Raises => {
            if !is_dotted_identifier(name) {
                return None;
            }
            (name.to_string(), None)
        }
        SectionKind::Returns | SectionKind::Yields => return None,
    };

    Some(SectionItemBuilder {
        display_name: Some(display_name),
        ty,
        description_lines: vec![DescriptionLine::normalized(description)],
    })
}

fn parse_google_return_item(
    kind: SectionKind,
    body: &[ParsedLine<'_>],
) -> Option<Vec<SectionItem>> {
    let mut lines = body.iter().skip_while(|line| line.text.trim().is_empty());
    let first_line = lines.next()?;
    let first = first_line.text.trim();
    if formats::google::is_section_like_header(first) {
        return None;
    }

    let (ty, first_description) = split_google_return_type(first)
        .map_or((None, first), |(ty, description)| (Some(ty), description));

    let mut description_lines = vec![DescriptionLine::normalized(first_description)];
    let mut preformatted_blocks = PreformattedBlockScanner::default();
    if !preformatted_blocks.consume_preformatted_line(first_line.text) {
        preformatted_blocks.observe_non_preformatted_line(first_line.text);
    }

    for line in lines {
        if preformatted_blocks.consume_preformatted_line(line.text) {
            description_lines.push(DescriptionLine::Source(line.text.to_string()));
            continue;
        }
        if formats::google::is_section_like_header(line.text.trim()) {
            return None;
        }
        preformatted_blocks.observe_non_preformatted_line(line.text);
        description_lines.push(DescriptionLine::Source(line.text.to_string()));
    }

    let description = normalize_description(description_lines);
    Some(vec![SectionItem::new(kind, None, ty, &description)])
}

fn split_google_return_type(line: &str) -> Option<(&str, &str)> {
    let (ty, description) = split_once_google_field_colon(line)?;
    let ty = ty.trim();
    if is_uri_scheme_prefix(ty, description) {
        return None;
    }

    let description = description.trim();

    is_docstring_type_expression(ty).then_some((ty, description))
}

fn split_once_google_field_colon(line: &str) -> Option<(&str, &str)> {
    let mut start = 0;

    while start < line.len() {
        let (before_colon, after_colon) = split_once_unbracketed_colon(line.get(start..)?)?;
        let colon = start + before_colon.len();
        // Napoleon accepts reST roles in type text; their colons are not field separators.
        if let Some(role_end) = rst_role_markup_end(line, colon) {
            start = role_end;
            continue;
        }

        return Some((&line[..colon], after_colon));
    }

    None
}

fn rst_role_markup_end(line: &str, start: usize) -> Option<usize> {
    let rest = line.get(start..)?;
    let after_initial_colon = rest.strip_prefix(':')?;
    let role_end = after_initial_colon.find(":`")?;
    let role = &after_initial_colon[..role_end];
    if role.is_empty()
        || !role
            .chars()
            .all(|char| char.is_ascii_alphanumeric() || matches!(char, ':' | '_' | '-' | '.'))
    {
        return None;
    }

    let content_start = start + ':'.len_utf8() + role_end + ":`".len();
    let content = line.get(content_start..)?;
    let closing_backtick = content.find('`')?;
    Some(content_start + closing_backtick + '`'.len_utf8())
}

fn is_google_parameter_display_name(display_name: &str) -> bool {
    display_name
        .split(',')
        .all(|name| is_google_parameter_name(name.trim()))
}

fn is_google_parameter_name(name: &str) -> bool {
    let identifier = name
        .strip_prefix("**")
        .or_else(|| name.strip_prefix('*'))
        .unwrap_or(name);

    is_identifier(identifier)
}

fn is_google_attribute_display_name(display_name: &str) -> bool {
    display_name
        .split(',')
        .all(|name| is_dotted_identifier(name.trim()))
}

fn is_dotted_identifier(name: &str) -> bool {
    !name.is_empty() && name.split('.').all(is_identifier)
}
