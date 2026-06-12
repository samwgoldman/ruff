use std::borrow::Cow;

use ruff_text_size::{TextRange, TextSize};

use super::markdown::MarkdownFence;

/// A parsed section ready for Markdown rendering.
///
/// Parser modules create one of these for each supported source section or
/// field list, then the docstring renderer places it back into the surrounding
/// raw source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::docstring) struct Section {
    range: TextRange,
    items: Vec<SectionItem>,
}

impl Section {
    /// Creates a section block from the items parsed out of one source section.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by follow-up structured docstring parsers")
    )]
    pub(in crate::docstring) fn new(range: TextRange, items: Vec<SectionItem>) -> Option<Self> {
        if items.is_empty() || items.iter().any(SectionItem::is_empty) {
            return None;
        }

        Some(Self { range, items })
    }

    pub(in crate::docstring) fn start(&self) -> TextSize {
        self.range.start()
    }

    pub(in crate::docstring) fn end(&self) -> TextSize {
        self.range.end()
    }

    /// Renders the section as Markdown into the given buffer.
    pub(in crate::docstring) fn render_markdown(
        &self,
        output: &mut String,
    ) -> MarkdownBoundary<'_> {
        let mut last_boundary = MarkdownBoundary::None;
        let mut rendered_section = false;
        for &(kind, heading) in SECTION_ORDER {
            if let Some(boundary) = render_markdown_section(
                output,
                heading,
                self.items.iter().filter(move |item| item.kind == kind),
                rendered_section,
            ) {
                rendered_section = true;
                last_boundary = boundary;
            }
        }
        last_boundary
    }
}

/// One display item within a structured docstring section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::docstring) struct SectionItem {
    kind: SectionKind,
    display_name: Option<String>,
    ty: Option<String>,
    description: String,
}

impl SectionItem {
    /// Creates a section item from parser-prepared name, type, and description parts.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by follow-up structured docstring parsers")
    )]
    pub(in crate::docstring) fn new(
        kind: SectionKind,
        display_name: Option<&str>,
        ty: Option<&str>,
        description: &str,
    ) -> Self {
        Self {
            kind,
            display_name: display_name.map(str::to_string),
            ty: ty.map(str::to_string),
            description: description.to_string(),
        }
    }

    /// Returns whether the item would render no user-visible Markdown.
    pub(in crate::docstring) fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.ty.as_deref().is_none_or(str::is_empty)
            && self.description.is_empty()
    }

    fn render(&self, output: &mut String) {
        let mut has_label = false;

        if let Some(name) = self.display_name.as_deref() {
            render_code_span(output, name);
            has_label = true;
        }

        if let Some(ty) = self.ty.as_deref()
            && !ty.is_empty()
        {
            if has_label {
                output.push_str(" (");
                render_type_code_span(output, ty);
                output.push(')');
            } else {
                render_type_code_span(output, ty);
                has_label = true;
            }
        }

        if !self.description.is_empty() {
            let description = self.description.as_str();
            let block_start = description_block_start(description);

            match block_start {
                Some(0) => {
                    if has_label {
                        output.push_str(":\n\n");
                    }
                    output.push_str(description);
                }
                Some(block_start) => {
                    let before_block = description[..block_start].trim_end_matches('\n');
                    if has_label {
                        output.push_str(": ");
                        render_inline_description(output, before_block);
                    } else {
                        output.push_str(before_block);
                    }
                    output.push_str("\n\n");
                    output.push_str(&description[block_start..]);
                }
                None => {
                    if has_label {
                        output.push_str(": ");
                        render_inline_description(output, description);
                    } else {
                        output.push_str(description);
                    }
                }
            }
        }
    }
}

/// Canonical docstring sections shared by supported formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::docstring) enum SectionKind {
    Parameters,
    KeywordArguments,
    OtherParameters,
    Attributes,
    Returns,
    Yields,
    Raises,
}

/// Identifies a Markdown construct that is still active at the boundary between paragraphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::docstring) enum MarkdownBoundary<'a> {
    /// An open Markdown code fence which must be closed before rendering more fields.
    Fence(MarkdownFence<'a>),
    /// A doctest prompt block which must be closed with a double newline.
    Doctest,
    /// A list which must be separated by a double newline to prevent a lazy
    /// continuation in a later paragraph.
    ListItem,
    /// No special boundary handling is needed.
    None,
}

impl<'a> MarkdownBoundary<'a> {
    fn scan(description: &'a str) -> Self {
        let mut boundary = Self::None;

        for line in description.lines().map(|line| line.trim_start_matches(' ')) {
            boundary.consume_line(line);
        }

        boundary
    }

    fn consume_line(&mut self, line: &'a str) {
        match *self {
            Self::Fence(fence) => {
                if fence.is_closed_by(line) {
                    *self = Self::None;
                }
            }
            Self::Doctest => {
                if line.is_empty() {
                    *self = Self::None;
                }
            }
            Self::None | Self::ListItem => {
                if line.is_empty() {
                    *self = Self::None;
                } else if starts_with_doctest_prompt(line) {
                    *self = Self::Doctest;
                } else if let Some(fence) = MarkdownFence::find(line) {
                    *self = Self::Fence(fence);
                } else if starts_with_markdown_list_item(line) {
                    *self = Self::ListItem;
                }
            }
        }
    }
}

pub(in crate::docstring) fn render_boundary_after_description(
    output: &mut String,
    boundary: MarkdownBoundary<'_>,
    trailing_source: Option<&str>,
) {
    match boundary {
        MarkdownBoundary::Fence(_) | MarkdownBoundary::Doctest | MarkdownBoundary::ListItem => {
            push_missing_blank_boundary(output, trailing_source);
        }
        MarkdownBoundary::None => {
            if !trailing_source.is_some_and(|raw| raw.starts_with('\n')) {
                output.push('\n');
            }
        }
    }
}

fn push_missing_blank_boundary(output: &mut String, trailing_source: Option<&str>) {
    if trailing_source.is_some_and(|raw| raw.starts_with("\n\n")) {
        return;
    }

    if trailing_source.is_some_and(|raw| raw.starts_with('\n')) {
        output.push('\n');
    } else {
        output.push_str("\n\n");
    }
}

const SECTION_ORDER: &[(SectionKind, &str)] = &[
    (SectionKind::Parameters, "Parameters"),
    (SectionKind::KeywordArguments, "Keyword Arguments"),
    (SectionKind::OtherParameters, "Other Parameters"),
    (SectionKind::Attributes, "Attributes"),
    (SectionKind::Returns, "Returns"),
    (SectionKind::Yields, "Yields"),
    (SectionKind::Raises, "Raises"),
];

fn render_markdown_section<'a>(
    output: &mut String,
    heading: &str,
    fields: impl Iterator<Item = &'a SectionItem>,
    rendered_previous_section: bool,
) -> Option<MarkdownBoundary<'a>> {
    let mut previous_boundary = None;

    for field in fields {
        if previous_boundary.is_none() {
            if rendered_previous_section {
                output.push_str("\n\n");
            }

            output.push_str("## ");
            output.push_str(heading);
            output.push('\n');
        }

        if let Some(boundary) = previous_boundary {
            render_separator_after_description(output, boundary);
        }

        field.render(output);
        previous_boundary = Some(MarkdownBoundary::scan(field.description.as_str()));
    }

    if let Some(boundary) = previous_boundary {
        render_section_end_after_description(output, boundary);
    }

    previous_boundary
}

fn render_separator_after_description(output: &mut String, boundary: MarkdownBoundary<'_>) {
    match boundary {
        MarkdownBoundary::Fence(fence) => {
            output.push('\n');
            output.push_str(fence.marker());
            output.push_str("\n\n");
        }
        MarkdownBoundary::Doctest | MarkdownBoundary::ListItem => {
            // Add an extra newline to keep the next field out of an open block.
            output.push_str("\n\n");
        }
        MarkdownBoundary::None => output.push('\n'),
    }
}

fn render_section_end_after_description(output: &mut String, boundary: MarkdownBoundary<'_>) {
    if let MarkdownBoundary::Fence(fence) = boundary {
        output.push('\n');
        output.push_str(fence.marker());
    }
}

/// Returns the byte offset where `description` first needs block-style rendering.
fn description_block_start(description: &str) -> Option<usize> {
    let mut offset = 0;

    for line in description.split_inclusive('\n') {
        let line_without_newline = line.strip_suffix('\n').unwrap_or(line);
        if line_starts_block_content(line_without_newline) {
            return Some(offset);
        }

        offset += line.len();
    }

    None
}

fn line_starts_block_content(line: &str) -> bool {
    let line = line.trim_start_matches(' ');
    MarkdownFence::find(line).is_some()
        || starts_with_doctest_prompt(line)
        || starts_with_markdown_list_item(line)
}

fn starts_with_doctest_prompt(line: &str) -> bool {
    line.starts_with(">>>")
}

fn starts_with_markdown_list_item(line: &str) -> bool {
    starts_with_unordered_markdown_list_item(line) || starts_with_ordered_markdown_list_item(line)
}

/// Returns whether `line` is exactly `-`, `+`, or `*`, or begins with one of
/// those markers followed by whitespace.
fn starts_with_unordered_markdown_list_item(line: &str) -> bool {
    matches!(
        line.as_bytes(),
        [b'-' | b'+' | b'*'] | [b'-' | b'+' | b'*', b' ' | b'\t', ..]
    )
}

/// Returns whether `line` begins with one to nine ASCII digits followed by
/// `.` or `)`, then whitespace or the end of the line.
fn starts_with_ordered_markdown_list_item(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut digit_count = 0;

    for byte in bytes {
        if digit_count < 9 && byte.is_ascii_digit() {
            digit_count += 1;
            continue;
        }

        if digit_count > 0 && matches!(*byte, b'.' | b')') {
            return bytes
                .get(digit_count + 1)
                .is_none_or(|byte| matches!(*byte, b' ' | b'\t'));
        }

        return false;
    }

    false
}

/// Renders description text that continues after a rendered name or type.
///
/// Multi-line descriptions are indented so continuation lines stay attached to
/// the preceding inline text.
fn render_inline_description(output: &mut String, description: &str) {
    if description.contains('\n') {
        output.push_str(&description.replace('\n', "\n    "));
    } else {
        output.push_str(description);
    }
}

/// Wraps `text` in a Markdown code span and appends it to output.
fn render_code_span(output: &mut String, text: &str) {
    // This chooses the number of backticks that we use to delimit the start and
    // end of the inline Markdown code span.
    //
    // The number we pick is one greater than the longest run of consecutive
    // backticks in `text`, which guarantees that we can wrap `text` unambiguously.
    let delimiter_len = text
        .split(|char| char != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
        + 1;

    output.extend(std::iter::repeat_n('`', delimiter_len));
    if text.starts_with('`') || text.ends_with('`') {
        // Per the CommonMark spec, wrap the contents of the code span in
        // whitespace if those contents start or end with backticks.
        //
        // <https://spec.commonmark.org/0.31.2/#code-spans>
        output.push(' ');
        output.push_str(text);
        output.push(' ');
    } else {
        output.push_str(text);
    }
    output.extend(std::iter::repeat_n('`', delimiter_len));
}

fn render_type_code_span(output: &mut String, ty: &str) {
    let normalized = normalize_type_for_code_span(ty);
    render_code_span(output, normalized.as_ref());
}

/// Normalizes type text so it fits in a single Markdown code span.
///
/// One-line types are returned unchanged. Multi-line types are trimmed line by
/// line, with empty lines discarded and remaining lines joined by a single
/// space.
///
/// For example:
///
/// ```python
/// dict[str,
///     object]
/// ```
///
/// becomes `dict[str, object]`.
fn normalize_type_for_code_span(ty: &str) -> Cow<'_, str> {
    if !ty.contains('\n') {
        return Cow::Borrowed(ty);
    }

    let mut normalized = String::new();
    for line in ty.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(line);
    }

    Cow::Owned(normalized)
}

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;
    use ruff_text_size::TextRange;

    use super::{Section, SectionItem, SectionKind};

    #[test]
    fn sections_render_in_canonical_order() {
        let section = section_block(vec![
            SectionItem::new(
                SectionKind::Raises,
                Some("ValueError"),
                None,
                "Invalid value.",
            ),
            SectionItem::new(
                SectionKind::Parameters,
                Some("value"),
                Some("str"),
                "The value.",
            ),
            SectionItem::new(
                SectionKind::OtherParameters,
                Some("kw_only"),
                Some("str"),
                "Less common option.",
            ),
            SectionItem::new(
                SectionKind::KeywordArguments,
                Some("limit"),
                Some("int"),
                "Maximum result count.",
            ),
            SectionItem::new(
                SectionKind::Returns,
                None,
                Some("bool"),
                "Whether validation passed.",
            ),
            SectionItem::new(
                SectionKind::Yields,
                None,
                Some("Iterator[int]"),
                "Generated values.",
            ),
            SectionItem::new(
                SectionKind::Attributes,
                Some("cache"),
                Some("dict[str,\n object]"),
                "Cached data.",
            ),
        ]);

        assert_snapshot!(render_markdown(&section), @"
        ## Parameters
        `value` (`str`): The value.

        ## Keyword Arguments
        `limit` (`int`): Maximum result count.

        ## Other Parameters
        `kw_only` (`str`): Less common option.

        ## Attributes
        `cache` (`dict[str, object]`): Cached data.

        ## Returns
        `bool`: Whether validation passed.

        ## Yields
        `Iterator[int]`: Generated values.

        ## Raises
        `ValueError`: Invalid value.
        ");
    }

    #[test]
    fn section_items_escape_code_span_labels() {
        let section = section_block(vec![SectionItem::new(
            SectionKind::Parameters,
            Some("`value`"),
            None,
            "Escaped label.",
        )]);

        assert_snapshot!(render_markdown(&section), @"
        ## Parameters
        `` `value` ``: Escaped label.
        ");
    }

    #[test]
    fn section_items_close_unterminated_fences() {
        let section = section_block(vec![SectionItem::new(
            SectionKind::Parameters,
            Some("example"),
            None,
            "```python\nprint('open')",
        )]);

        assert_snapshot!(render_markdown(&section), @"
        ## Parameters
        `example`:

        ```python
        print('open')
        ```
        ");
    }

    fn render_markdown(section: &Section) -> String {
        let mut output = String::new();
        section.render_markdown(&mut output);
        output
    }

    fn section_block(items: Vec<SectionItem>) -> Section {
        let Some(section) = Section::new(TextRange::default(), items) else {
            panic!("test section items should form a section block");
        };
        section
    }
}
