use std::borrow::Cow;

use ruff_text_size::{TextRange, TextSize};

use super::super::sections::{Section, render_boundary_after_description};

/// Accepts a PEP 257-trimmed docstring body and renders Markdown for sections
/// recognized in supported formats.
pub(super) fn render(body: &str) -> Cow<'_, str> {
    Docstring::parse(body).render_markdown()
}

/// A display-oriented parse of a PEP 257-trimmed docstring body.
struct Docstring<'a> {
    /// The input docstring body.
    source: &'a str,
    /// The structured segments that will be rendered in the final output.
    segments: Vec<Segment<'a>>,
}

impl<'a> Docstring<'a> {
    /// Factory  method that parses `source` into blocks for Markdown rendering.
    fn parse(source: &'a str) -> Self {
        Self {
            source,
            segments: parse_blocks(source, Vec::new()),
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

/// Builds the render block list from parsed sections.
///
/// Returns an empty list when there are no structural replacements to apply,
/// or when an invalid range should trigger a fall back to the original docstring.
fn parse_blocks(raw: &str, mut sections: Vec<Section>) -> Vec<Segment<'_>> {
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

    use super::{Docstring, parse_blocks};
    use crate::docstring::sections::{Section, SectionItem, SectionKind};

    #[test]
    fn docstrings_without_structured_sections_are_returned_unchanged() {
        let docstring = "Summary.\n\nDetails.";
        let parsed = Docstring::parse(docstring);

        assert_eq!(parsed.render_markdown(), docstring);
    }

    #[test]
    fn following_prose_does_not_continue_a_rendered_parameter_list() {
        let rendered = render_parameter_docstring("- First option.", "After.");

        assert_snapshot!(rendered, @"
        ## Parameters
        `value`:

        - First option.

        After.
        ");
    }

    #[test]
    fn following_prose_is_rendered_outside_a_parameter_doctest() {
        let rendered = render_parameter_docstring(">>> value\n1", "After.");

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
        let rendered = render_parameter_docstring("```python\nvalue = 1", "After.");

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
            segments: parse_blocks(
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

    fn render_parameter_docstring(description: &str, following_prose: &str) -> String {
        let section_source = format!(":param value:\n{}", indent_description(description));
        let raw = format!("{section_source}\n{following_prose}");

        Docstring {
            source: &raw,
            segments: parse_blocks(
                &raw,
                vec![section_block(
                    TextRange::up_to(TextSize::of(section_source.as_str())),
                    vec![SectionItem::new(
                        SectionKind::Parameters,
                        Some("value"),
                        None,
                        description,
                    )],
                )],
            ),
        }
        .render_markdown()
        .into_owned()
    }

    fn indent_description(description: &str) -> String {
        description
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn section_block(range: TextRange, items: Vec<SectionItem>) -> Section {
        let Some(section) = Section::new(range, items) else {
            panic!("test section items should form a section block");
        };
        section
    }
}
