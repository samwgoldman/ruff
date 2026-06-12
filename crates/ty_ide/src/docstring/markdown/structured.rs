use std::borrow::Cow;

use ruff_text_size::{TextRange, TextSize};

use super::super::preformatted::RestLiteralBlockScanner;
use super::super::sections::{
    Section, line_starts_markdown_block_content, render_boundary_after_description,
};

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
                Segment::Raw(raw) => output.push_str(normalize_raw_segment(raw).as_ref()),
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

/// Normalizes a raw segment by collapsing soft line breaks in its interior prose.
///
/// Raw segments include the source newlines that separate them from structured
/// sections, so this preserves leading and trailing newlines and only delegates
/// the prose between them to `normalize_prose_soft_line_breaks`.
fn normalize_raw_segment(raw: &str) -> Cow<'_, str> {
    if raw_segment_has_structural_content(raw) {
        return Cow::Borrowed(raw);
    }

    let leading_newlines = raw.len() - raw.trim_start_matches('\n').len();
    let trailing_newlines = raw.len() - raw.trim_end_matches('\n').len();
    let prose_start = leading_newlines;
    let prose_end = raw.len() - trailing_newlines;

    // Boundary-only segments have no interior prose to normalize.
    if prose_start >= prose_end {
        return Cow::Borrowed(raw);
    }

    let prose = &raw[prose_start..prose_end];
    let Cow::Owned(prose) = normalize_prose_soft_line_breaks(prose) else {
        return Cow::Borrowed(raw);
    };

    let mut output = String::with_capacity(raw.len());
    output.push_str(&raw[..prose_start]);
    output.push_str(&prose);
    output.push_str(&raw[prose_end..]);
    Cow::Owned(output)
}

fn normalize_prose_soft_line_breaks(prose: &str) -> Cow<'_, str> {
    // Only rewrite multi-line prose; block-like content keeps its source line breaks.
    if !prose.contains('\n') || prose_has_block_content(prose) {
        return Cow::Borrowed(prose);
    }

    let mut output = String::with_capacity(prose.len());
    for line in prose.lines() {
        let line = line.trim();
        let is_blank = line.is_empty();
        let has_open_paragraph = !output.is_empty() && !output.ends_with("\n\n");

        // A blank line after prose closes the current paragraph.
        if is_blank && has_open_paragraph {
            output.push_str("\n\n");
            continue;
        }

        // Leading, repeated, or trailing blank lines don't add extra paragraph breaks.
        if is_blank {
            continue;
        }

        // Non-blank lines within a paragraph are source-level soft wraps.
        if has_open_paragraph {
            output.push(' ');
        }
        output.push_str(line);
    }

    Cow::Owned(output)
}

fn prose_has_block_content(prose: &str) -> bool {
    let mut literal_blocks = RestLiteralBlockScanner::default();

    for line in prose.lines() {
        if literal_blocks.consume_line(line) {
            return true;
        }

        let trimmed = line.trim_start();
        if !trimmed.is_empty() && line.starts_with(char::is_whitespace) {
            return true;
        }

        if line_starts_markdown_block_content(trimmed) || starts_with_rst_directive(trimmed) {
            return true;
        }

        literal_blocks.observe_marker_in_line(line);
    }

    false
}

/// Returns whether a raw segment's line breaks should be preserved.
///
/// The heuristic is intentionally conservative: non-empty indented lines may be
/// Google/NumPy-style blocks or other hand-formatted text, and reST section
/// adornments such as `-----` or `=====` can be part of heading syntax. In
/// either case, the raw segment is not plain prose, so its line breaks should
/// be preserved.
fn raw_segment_has_structural_content(raw: &str) -> bool {
    raw.lines().any(|line| {
        let trimmed = line.trim_start_matches(' ');
        !trimmed.is_empty() && (line.starts_with(' ') || is_rst_section_adornment(trimmed))
    })
}

/// Returns whether `line` could be a reST section title adornment.
///
/// Docutils specifies section adornments as a single repeated non-alphanumeric
/// printable 7-bit ASCII character:
/// <https://docutils.sourceforge.io/docs/ref/rst/restructuredtext.html#sections>
fn is_rst_section_adornment(line: &str) -> bool {
    let mut chars = line.chars();
    let Some(marker) = chars.next() else {
        return false;
    };

    line.len() >= 3 && marker.is_ascii_punctuation() && chars.all(|char| char == marker)
}

fn starts_with_rst_directive(line: &str) -> bool {
    line.starts_with(".. ")
}

/// A contiguous section of the final docstring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Segment<'a> {
    /// Source text between structured sections.
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

/// Records the non-empty source text in `range` as a raw segment.
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

    use super::{Docstring, normalize_raw_segment, parse_blocks};
    use crate::docstring::sections::{Section, SectionItem, SectionKind};

    #[test]
    fn docstrings_without_structured_sections_are_returned_unchanged() {
        let docstring = "Summary.\n\nDetails.";
        let parsed = Docstring::parse(docstring);

        assert_eq!(parsed.render_markdown(), docstring);
    }

    #[test]
    fn ordinary_prose_soft_line_breaks_are_collapsed() {
        let raw = "\
Intro wraps
onto the next source line.

Middle paragraph.

Outro wraps
too.
";
        let rendered = normalize_raw_segment(raw);

        assert_snapshot!(rendered.as_ref(), @"
        Intro wraps onto the next source line.

        Middle paragraph.

        Outro wraps too.
        ");
    }

    #[test]
    fn ordinary_prose_with_indented_blocks_is_preserved() {
        let raw = "\
Intro wraps
before block.

    code = 1
    print(code)

Outro wraps
too.
";
        let rendered = normalize_raw_segment(raw);

        assert_snapshot!(rendered.as_ref(), @"
        Intro wraps
        before block.

            code = 1
            print(code)

        Outro wraps
        too.
        ");
    }

    #[test]
    fn ordinary_prose_with_rst_section_adornment_is_preserved() {
        let raw = "\
Intro heading
.............

Outro wraps
too.
";
        let rendered = normalize_raw_segment(raw);

        assert_snapshot!(rendered.as_ref(), @"
        Intro heading
        .............

        Outro wraps
        too.
        ");
    }

    #[test]
    fn following_prose_does_not_continue_a_rendered_parameter_list() {
        let rendered = render_parameter_docstring("- First option.", "After.");

        assert_snapshot!(rendered, @"
        ## Parameters
        ```python
        value
        ```
        - First option.

        After.
        ");
    }

    #[test]
    fn following_prose_is_rendered_outside_a_parameter_doctest() {
        let rendered = render_parameter_docstring(">>> value\n1", "After.");

        assert_snapshot!(rendered, @"
        ## Parameters
        ```python
        value
        ```
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
        ```python
        value
        ```
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
