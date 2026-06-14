use std::borrow::Cow;

use ruff_text_size::{TextRange, TextSize};

mod rst;

use super::super::formats::Formats;
use super::super::sections::{Section, render_boundary_after_description};

/// Accepts a PEP 257-trimmed docstring body and renders Markdown for sections
/// recognized in supported formats.
pub(super) fn render<'a>(body: &'a str, formats: &Formats) -> Cow<'a, str> {
    Docstring::parse(body, formats).render_markdown()
}

/// A display-oriented parse of a PEP 257-trimmed docstring body.
struct Docstring<'a> {
    /// The input docstring body.
    source: &'a str,
    /// The structured segments that will be rendered in the final output.
    segments: Vec<Segment<'a>>,
}

impl<'a> Docstring<'a> {
    /// Factory method that parses `source` into blocks for Markdown rendering.
    fn parse(source: &'a str, formats: &Formats) -> Self {
        Self {
            source,
            segments: parse_blocks(source, formats),
        }
    }

    /// Renders the parsed docstring as Markdown, borrowing `self.source` when unchanged.
    fn render_markdown(&self) -> Cow<'a, str> {
        if self.segments.is_empty()
            || matches!(
                self.segments.as_slice(),
                [Segment::Raw(raw)] if *raw == self.source
            )
        {
            return Cow::Borrowed(self.source);
        }

        let mut output = String::new();
        for (index, block) in self.segments.iter().enumerate() {
            match block {
                Segment::Raw(raw) => output.push_str(raw),
                Segment::Structured(section) => {
                    let boundary = section.render_markdown(&mut output);
                    if let Some(next) = self.segments.get(index + 1) {
                        render_boundary_after_description(&mut output, boundary, next.as_raw());
                    }
                }
            }
        }

        Cow::Owned(output)
    }
}

/// A contiguous section of the final docstring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Segment<'a> {
    /// Source text that should be preserved exactly.
    Raw(&'a str),
    /// A portion of the docstring should be rendered structurally.
    Structured(Section),
}

impl Segment<'_> {
    fn as_raw(&self) -> Option<&str> {
        match self {
            Self::Raw(raw) => Some(raw),
            Self::Structured(_) => None,
        }
    }
}

/// Produces structured docstring sections from a parsed docstring format.
trait SectionSource {
    fn structured_sections(&self) -> Vec<Section>;
}

/// Builds the render block list from parsed sections.
///
/// Returns an empty list when there are no structural replacements to apply,
/// or when an invalid range should trigger a fall back to the original docstring.
fn parse_blocks<'a>(raw: &'a str, formats: &Formats) -> Vec<Segment<'a>> {
    parse_section_blocks(raw, formats.rst().structured_sections())
}

fn parse_section_blocks(raw: &str, mut sections: Vec<Section>) -> Vec<Segment<'_>> {
    sections.sort_by_key(Section::start);
    let raw_len = TextSize::of(raw);
    let mut blocks = Vec::new();
    let mut rendered_through = TextSize::default();

    for section in sections {
        let start = section.start();
        let end = section.end();

        if end > raw_len {
            return Vec::new();
        }

        if start < rendered_through {
            continue;
        }

        if !push_raw_block(&mut blocks, raw, TextRange::new(rendered_through, start)) {
            return Vec::new();
        }
        rendered_through = end;
        blocks.push(Segment::Structured(section));
    }

    if !blocks.is_empty()
        && !push_raw_block(&mut blocks, raw, TextRange::new(rendered_through, raw_len))
    {
        return Vec::new();
    }

    blocks
}

/// Appends the (non-empty) raw source text in `range` to `blocks`.
///
/// Returns `true` if `range` is empty or a valid slice of `raw`. Returns `false`
/// if the range is out of bounds or does not fall on UTF-8 boundaries.
fn push_raw_block<'a>(blocks: &mut Vec<Segment<'a>>, raw: &'a str, range: TextRange) -> bool {
    if range.is_empty() {
        return true;
    }

    let Some(raw) = raw.get(range.start().to_usize()..range.end().to_usize()) else {
        return false;
    };
    blocks.push(Segment::Raw(raw));
    true
}

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;
    use ruff_text_size::{TextRange, TextSize};

    use super::{Docstring, parse_section_blocks};
    use crate::docstring::formats::Formats;
    use crate::docstring::sections::{Section, SectionItem, SectionKind};

    #[test]
    fn docstrings_without_structured_sections_are_returned_unchanged() {
        let docstring = "Summary.\n\nDetails.";
        let formats = Formats::parse(docstring);
        let parsed = Docstring::parse(docstring, &formats);

        assert_eq!(parsed.render_markdown(), docstring);
    }

    #[test]
    fn following_prose_does_not_continue_a_rendered_parameter_list() {
        let rendered = render_docstring(":param value:\n    - First option.\nAfter.");

        assert_snapshot!(rendered, @"
        ## Parameters
        `value`:

        - First option.

        After.
        ");
    }

    #[test]
    fn following_prose_is_rendered_outside_a_parameter_doctest() {
        let rendered = render_docstring(":param value:\n    >>> value\n    1\nAfter.");

        assert_snapshot!(rendered, @"
        ## Parameters
        `value`:

        >>> value
        1

        After.
        ");
    }

    #[test]
    fn following_prose_is_rendered_outside_an_unclosed_parameter_code_fence() {
        let rendered = render_docstring(":param value:\n    ```python\n    value = 1\nAfter.");

        assert_snapshot!(rendered, @"
        ## Parameters
        `value`:

        ```python
        value = 1
        ```

        After.
        ");
    }

    #[test]
    fn invalid_section_end_falls_back_to_original_docstring() {
        let raw = "Summary.\n\n:param value:\n    Value.";
        let rendered = Docstring {
            source: raw,
            segments: parse_section_blocks(
                raw,
                vec![section_block(
                    TextRange::new(TextSize::from(10), TextSize::of(raw) + TextSize::from(1)),
                    vec![SectionItem::new(
                        SectionKind::Parameters,
                        Some("value"),
                        None,
                        "Value.",
                    )],
                )],
            ),
        }
        .render_markdown();

        assert_eq!(rendered, raw);
    }

    fn render_docstring(docstring: &str) -> String {
        let formats = Formats::parse(docstring);
        Docstring::parse(docstring, &formats)
            .render_markdown()
            .into_owned()
    }

    fn section_block(range: TextRange, items: Vec<SectionItem>) -> Section {
        let Some(section) = Section::new(range, items) else {
            panic!("test section items should form a section block");
        };
        section
    }
}
