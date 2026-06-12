use crate::docstring::formats::{
    self,
    numpy::{is_numpy_anonymous_return_type, is_numpy_item_name, split_numpy_type_separator},
};
use crate::docstring::parsing::{ParsedLine, split_once_unbracketed_colon};
use crate::docstring::sections::{Section, SectionKind};
use ruff_text_size::TextRange;

use super::{DescriptionLine, SectionItemBuilder, SectionSource, parse_named_items};

impl SectionSource for formats::numpy::Docstring<'_> {
    fn structured_sections(&self) -> Vec<Section> {
        self.sections()
            .iter()
            .filter_map(|section| {
                let kind = section.kind();
                numpy_section_block(kind, section.range(), section.body())
            })
            .collect()
    }
}

fn numpy_section_block(
    kind: SectionKind,
    range: TextRange,
    body: &[ParsedLine<'_>],
) -> Option<Section> {
    let items = match kind {
        SectionKind::Parameters
        | SectionKind::KeywordArguments
        | SectionKind::OtherParameters
        | SectionKind::Attributes => parse_named_items(kind, body, parse_numpy_named_item)?,
        SectionKind::Returns | SectionKind::Yields => {
            parse_named_items(kind, body, parse_numpy_return_item)?
        }
        SectionKind::Raises => parse_named_items(kind, body, parse_numpy_raise_item)?,
    };

    Section::new(range, items)
}

fn parse_numpy_named_item(line: &str) -> Option<SectionItemBuilder> {
    let (name, ty) = if let Some((name, ty)) = split_numpy_type_separator(line) {
        (name, Some(ty))
    } else {
        let name = line.trim();
        is_numpy_item_name(name).then_some((name, None))?
    };

    Some(SectionItemBuilder {
        display_name: Some(name.to_string()),
        ty: ty.map(str::to_string),
        description_lines: Vec::new(),
    })
}

fn parse_numpy_return_item(line: &str) -> Option<SectionItemBuilder> {
    if let Some((name, ty)) = split_numpy_type_separator(line) {
        return Some(SectionItemBuilder {
            display_name: Some(name.to_string()),
            ty: Some(ty.to_string()),
            description_lines: Vec::new(),
        });
    }
    if has_numpy_named_return_separator(line) {
        return None;
    }

    is_numpy_anonymous_return_type(line).then(|| SectionItemBuilder {
        display_name: None,
        ty: Some(line.to_string()),
        description_lines: Vec::new(),
    })
}

fn has_numpy_named_return_separator(line: &str) -> bool {
    split_once_unbracketed_colon(line)
        .is_some_and(|(name, _)| name.chars().last().is_some_and(char::is_whitespace))
}

fn parse_numpy_raise_item(line: &str) -> Option<SectionItemBuilder> {
    let (name, description) = line
        .split_once(':')
        .map_or((line.trim(), None), |(name, description)| {
            (name.trim(), Some(description.trim()))
        });
    if !is_numpy_item_name(name) {
        return None;
    }
    let description_lines = match description {
        Some(description) if !description.is_empty() => {
            vec![DescriptionLine::normalized(description)]
        }
        Some(_) | None => Vec::new(),
    };

    Some(SectionItemBuilder {
        display_name: Some(name.to_string()),
        ty: None,
        description_lines,
    })
}
