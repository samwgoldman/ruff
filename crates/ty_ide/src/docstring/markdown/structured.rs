use std::borrow::Cow;

use ruff_text_size::{TextRange, TextSize};

mod google;
mod numpy;
mod rst;

use super::super::formats::Formats;
use super::super::parsing::{ParsedLine, indentation};
use super::super::preformatted::{PreformattedBlockScanner, RestLiteralBlockScanner};
use super::super::sections::{
    Section, SectionItem, SectionKind, line_starts_markdown_block_content,
    render_boundary_after_description,
};

/// Accepts a PEP 257-trimmed docstring body and renders Markdown for sections
/// recognized in supported formats.
pub(super) fn render<'a>(body: &'a str, formats: &Formats<'_>) -> Cow<'a, str> {
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
    fn parse(source: &'a str, formats: &Formats<'_>) -> Self {
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

/// Produces structured docstring sections from a parsed docstring format.
trait SectionSource {
    fn structured_sections(&self) -> Vec<Section>;
}

/// Builds the render block list from parsed sections.
///
/// Returns an empty list when there are no structural replacements to apply,
/// or when an invalid range should trigger a fall back to the original docstring.
fn parse_blocks<'a>(raw: &'a str, formats: &Formats<'_>) -> Vec<Segment<'a>> {
    let mut sections = formats.rst().structured_sections();
    sections.extend(formats.google().structured_sections());
    sections.extend(formats.numpy().structured_sections());
    parse_section_blocks(raw, sections)
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

pub(super) struct SectionItemBuilder {
    display_name: Option<String>,
    ty: Option<String>,
    description_lines: Vec<DescriptionLine>,
}

impl SectionItemBuilder {
    pub(super) fn finish(self, kind: SectionKind) -> SectionItem {
        let description = normalize_description(self.description_lines);
        SectionItem::new(
            kind,
            self.display_name.as_deref(),
            self.ty.as_deref(),
            &description,
        )
    }

    pub(super) fn push_description(&mut self, line: &str) {
        self.description_lines
            .push(DescriptionLine::Source(line.to_string()));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DescriptionLine {
    Normalized(String),
    Source(String),
}

impl DescriptionLine {
    pub(super) fn normalized(line: &str) -> Self {
        Self::Normalized(line.trim().to_string())
    }
}

pub(super) fn parse_named_items(
    kind: SectionKind,
    body: &[ParsedLine<'_>],
    mut parse_item: impl FnMut(&str) -> Option<SectionItemBuilder>,
) -> Option<Vec<SectionItem>> {
    let mut items = Vec::new();
    let mut current: Option<SectionItemBuilder> = None;
    let mut item_indent = None;
    let mut preformatted_blocks = PreformattedBlockScanner::default();

    for line in body {
        if preformatted_blocks.consume_preformatted_line(line.text) {
            let item_indent = item_indent?;
            if !line.text.trim().is_empty() && indentation(line.text) <= item_indent {
                return None;
            }

            let current = current.as_mut()?;
            current.push_description(line.text);
            continue;
        }

        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            if let Some(current) = &mut current {
                current.push_description("");
            }
            continue;
        }

        let line_indent = indentation(line.text);
        if item_indent.is_none_or(|indent| line_indent == indent) {
            if let Some(item) = parse_item(trimmed) {
                if let Some(current) = current.replace(item) {
                    items.push(current.finish(kind));
                }
                item_indent.get_or_insert(line_indent);
                preformatted_blocks.observe_non_preformatted_line(line.text);
                continue;
            }
            if item_indent.is_some() {
                return None;
            }
        }
        if item_indent.is_some_and(|indent| line_indent < indent) {
            return None;
        }

        let current = current.as_mut()?;
        current.push_description(line.text);
        preformatted_blocks.observe_non_preformatted_line(line.text);
    }

    if let Some(current) = current {
        items.push(current.finish(kind));
    }
    (!items.is_empty()).then_some(items)
}

pub(super) fn is_uri_scheme_prefix(ty: &str, description: &str) -> bool {
    if !is_uri_scheme(ty) {
        return false;
    }

    let Some(first) = description.chars().next() else {
        return false;
    };
    if first.is_whitespace() {
        return false;
    }

    matches!(first, '/' | '?' | '#' | '@' | ':')
}

fn is_uri_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    chars.next().is_some_and(|char| char.is_ascii_alphabetic())
        && chars.all(|char| char.is_ascii_alphanumeric() || matches!(char, '+' | '-' | '.'))
}

pub(super) fn normalize_description(lines: Vec<DescriptionLine>) -> String {
    let dedent = lines
        .iter()
        .filter_map(|line| match line {
            DescriptionLine::Source(line) if !line.trim().is_empty() => Some(indentation(line)),
            DescriptionLine::Normalized(_) | DescriptionLine::Source(_) => None,
        })
        .min()
        .unwrap_or(0);

    let mut description = lines
        .into_iter()
        .map(|line| match line {
            DescriptionLine::Normalized(line) => line,
            DescriptionLine::Source(line) => {
                strip_indentation(&line, dedent).trim_end().to_string()
            }
        })
        .skip_while(String::is_empty)
        .collect::<Vec<_>>();
    while description.last().is_some_and(String::is_empty) {
        description.pop();
    }
    let description = description.join("\n");
    match normalize_prose_soft_line_breaks(&description) {
        Cow::Borrowed(_) => description,
        Cow::Owned(description) => description,
    }
}

pub(super) fn strip_indentation(line: &str, width: usize) -> &str {
    let mut indentation_width = 0;
    for (index, char) in line.char_indices() {
        let char_width = match char {
            ' ' => 1,
            '\t' => 8,
            _ => return &line[index..],
        };

        if indentation_width + char_width > width {
            return &line[index..];
        }

        indentation_width += char_width;
        if indentation_width == width {
            return &line[index + char.len_utf8()..];
        }
    }

    ""
}

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;
    use ruff_text_size::{TextRange, TextSize};

    use super::{Docstring, normalize_raw_segment, parse_section_blocks};
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
        let rendered = render_docstring(":param value:\n    - First option.\nAfter.");

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
    fn google_sections_render_markdown_sections() {
        let docstring = "\
Summary.

Args:
    value (str): The value.
        More detail.
    *items: Extra items.

Keyword Args:
    optional (int): Optional value.

Returns:
    bool: Whether validation passed.

Yields:
    int: Next value.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        Summary.

        ## Parameters
        ```python
        value: str
        ```
        The value. More detail.

        ```python
        *items
        ```
        Extra items.

        ## Keyword Arguments
        ```python
        optional: int
        ```
        Optional value.

        ## Returns
        ```python
        bool
        ```
        Whether validation passed.

        ## Yields
        ```python
        int
        ```
        Next value.
        ");

        let docstring = "\
Args:
    x, y: Coordinates.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Parameters
        ```python
        x, y
        ```
        Coordinates.
        ");

        let docstring = "\
Keyword Arguments:
    retries: Retry count.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Keyword Arguments
        ```python
        retries
        ```
        Retry count.
        ");

        let docstring = "\
Args:
    value: The value.
Additional details.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Parameters
        ```python
        value
        ```
        The value.
        Additional details.
        ");

        let docstring = "\
Args:
    value: The value.
Methods:
    work: Does work.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Parameters
        ```python
        value
        ```
        The value.
        Methods:
            work: Does work.
        ");

        let docstring = "\
Returns:
    bool: Whether validation passed.
Additional details.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Returns
        ```python
        bool
        ```
        Whether validation passed.
        Additional details.
        ");

        let docstring = "\
Returns:
    str | None: Optional value.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Returns
        ```python
        str | None
        ```
        Optional value.
        ");

        let docstring = "\
Returns:
    One of the known values: foo or bar.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Returns
        One of the known values: foo or bar.
        ");

        let docstring = "\
Returns:
    True if it succeeded.
    False otherwise.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Returns
        True if it succeeded. False otherwise.
        ");

        let docstring = "\
Yields:
    The next item.
    Nothing when exhausted.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Yields
        The next item. Nothing when exhausted.
        ");

        let docstring = "\
Returns:
    str:Path/to/file.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Returns
        ```python
        str
        ```
        Path/to/file.
        ");

        let docstring = "\
Returns:
    Path:foo@bar.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Returns
        ```python
        Path
        ```
        foo@bar.
        ");

        let docstring = "\
Returns:
    https://example.com/path
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Returns
        https://example.com/path
        ");

        let docstring = "\
Yields:
    :obj:`list` of :obj:`str`: Result chunks.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Yields
        ```python
        :obj:`list` of :obj:`str`
        ```
        Result chunks.
        ");

        let docstring = "\
Returns:
    str: Example output.
        ```python
        Args:
            value: still code.
        Returns:
            still code.
        ```
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Returns
        ```python
        str
        ```
        Example output.
        ```python
        Args:
            value: still code.
        Returns:
            still code.
        ```
        ");

        let docstring = "\
Yields:
    int: Example output.
        Example::
            Args:
                still code.
            Yields:
                still code.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Yields
        ```python
        int
        ```
        Example output.
        Example::
            Args:
                still code.
            Yields:
                still code.
        ");
    }

    #[test]
    fn unsupported_google_sections_stay_raw() {
        let docstring = "\
Summary.

Args:
    Inputs are normalized first.
    value: The value.
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);

        let docstring = "\
Summary.

Examples:
    Args:
        value: demo input.
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);

        let docstring = "\
Summary.

Returns:
    bool: Whether validation passed.

    Examples:
        Use it.
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);

        let docstring = "\
Summary.

Yields:
    int: Next value.

    Examples:
        Use it.
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);

        let docstring = "\
Summary.

Args:
    Inputs are normalized first.
    Args:
        value: demo input.
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);

        let docstring = "\
Summary.

Args:
    value: The value.

    Examples:
        Use it.
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);

        let docstring = "\
Summary.

Returns:
    Examples:
        Use it.
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);

        let docstring = "\
Summary.

Args:
    value: Example.
        ```python

Args:
    nested = 1
        ```
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);
    }

    #[test]
    fn numpy_sections_render_markdown_sections() {
        let docstring = "\
Summary.

Parameters
----------
value, alias : str
    The value.
other
    Another value.

Other Parameters
----------------
kw_only : str, optional
    Less common option.

Returns
-------
    result : bool
        Whether validation passed.

Yields
------
    int
        Next value.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        Summary.

        ## Parameters
        ```python
        value, alias: str
        ```
        The value.

        ```python
        other
        ```
        Another value.

        ## Other Parameters
        ```python
        kw_only: str, optional
        ```
        Less common option.

        ## Returns
        ```python
        result: bool
        ```
        Whether validation passed.

        ## Yields
        ```python
        int
        ```
        Next value.
        ");

        let docstring = "\
Summary.

Parameters
----------
value: str
    The value.

Returns
-------
result: bool
    Whether validation passed.

Yields
------
item: int
    Next value.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        Summary.

        ## Parameters
        ```python
        value: str
        ```
        The value.

        ## Returns
        ```python
        result: bool
        ```
        Whether validation passed.

        ## Yields
        ```python
        item: int
        ```
        Next value.
        ");

        let docstring = "\
Summary.

Returns
-------
    :obj:`list` of :obj:`str`
        Primary values.
    list of node-like
        Related nodes.

Yields
------
    :class:`Iterator` of :obj:`str`
        Next labels.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        Summary.

        ## Returns
        ```python
        :obj:`list` of :obj:`str`
        ```
        Primary values.

        ```python
        list of node-like
        ```
        Related nodes.

        ## Yields
        ```python
        :class:`Iterator` of :obj:`str`
        ```
        Next labels.
        ");
    }

    #[test]
    fn unsupported_numpy_sections_stay_raw() {
        let docstring = "\
Summary.

Returns
-------
    The created object.
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);

        let docstring = "\
Summary.

Parameters
----------
value : str
    Example:
    ```python
other : str
    ```
other : int
    Real parameter.
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);
    }

    #[test]
    fn indented_sections_stay_raw() {
        let docstring = "\
Summary.

    Args:
        value: The value.

    Parameters
    ----------
    other : str
        Another value.
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);
    }

    #[test]
    fn following_prose_is_rendered_outside_a_parameter_doctest() {
        let rendered = render_docstring(":param value:\n    >>> value\n    1\nAfter.");

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
        let rendered = render_docstring(":param value:\n    ```python\n    value = 1\nAfter.");

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
        parse_docstring(docstring).render_markdown().into_owned()
    }

    fn parse_docstring(raw: &str) -> Docstring<'_> {
        let formats = Formats::parse(raw);
        Docstring::parse(raw, &formats)
    }

    fn section_block(range: TextRange, items: Vec<SectionItem>) -> Section {
        let Some(section) = Section::new(range, items) else {
            panic!("test section items should form a section block");
        };
        section
    }
}
