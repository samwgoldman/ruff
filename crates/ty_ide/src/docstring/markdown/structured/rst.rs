use ruff_text_size::{TextRange, TextSize};
use rustc_hash::FxHashMap;

use crate::docstring::formats::rst;
use crate::docstring::sections::{Section, SectionItem, SectionKind};

use super::SectionSource;

impl SectionSource for rst::Docstring {
    fn structured_sections(&self) -> Vec<Section> {
        let mut sections = Vec::new();

        for field_list in self.field_lists() {
            if field_list.indent() != TextSize::default() {
                continue;
            }

            let Some(section) = RenderPlan::from_fields(field_list.fields())
                .and_then(|plan| plan.execute(field_list.range()))
            else {
                continue;
            };

            sections.push(section);
        }

        sections
    }
}

/// Validates a reST field list and stores cross-field metadata needed while rendering.
struct RenderPlan<'a> {
    fields: &'a [rst::Field],
    parameter_types: SupplementalTypeFields<'a>,
    attribute_types: SupplementalTypeFields<'a>,
    return_type: Option<&'a str>,
    has_returns: bool,
}

impl<'a> RenderPlan<'a> {
    /// Validates `fields` and builds a plan for structured rendering.
    ///
    /// This factory conservatively returns `None` when we aren't certain that
    /// we can interpret the field list correctly. This indicates to the caller
    /// that the field list should be left raw rather than replaced with
    /// Markdown.
    fn from_fields(fields: &'a [rst::Field]) -> Option<Self> {
        let mut has_returns = false;
        let mut parameter_types = SupplementalTypeFields::default();
        let mut attribute_types = SupplementalTypeFields::default();
        let mut return_type = None;

        for field in fields {
            match field {
                rst::Field::Parameter {
                    lookup_name, ty, ..
                } => {
                    parameter_types.record_value_field(lookup_name.as_str(), ty.is_some());
                }
                rst::Field::Attribute { name, ty, .. } => {
                    attribute_types.record_value_field(name.as_str(), ty.is_some());
                }
                rst::Field::Returns { .. } => {
                    has_returns = true;
                }
                rst::Field::Raises { .. } => {}
                rst::Field::ParameterType { lookup_name, ty } => {
                    parameter_types.record_type_field(lookup_name.as_str(), ty.as_str())?;
                }
                rst::Field::AttributeType { name, ty } => {
                    attribute_types.record_type_field(name.as_str(), ty.as_str())?;
                }
                rst::Field::ReturnType { ty } => {
                    if return_type.replace(ty.as_str()).is_some() {
                        return None;
                    }
                }
                rst::Field::Metadata => {
                    // Sphinx metadata fields are not user-facing hover sections.
                }
                rst::Field::Unknown { .. } => {
                    // Unknown or unsupported fields may have section semantics we
                    // do not understand, so leave the full field list unstructured.
                    return None;
                }
            }
        }

        if !parameter_types.all_types_match_value_fields() {
            return None;
        }

        if !attribute_types.all_types_match_value_fields() {
            return None;
        }

        Some(Self {
            fields,
            parameter_types,
            attribute_types,
            return_type,
            has_returns,
        })
    }

    /// Attempts to render the validated field list into a section block.
    fn execute(&self, range: TextRange) -> Option<Section> {
        Section::new(range, self.items())
    }

    fn items(&self) -> Vec<SectionItem> {
        let mut items = Vec::new();

        for field in self.fields {
            match field {
                rst::Field::Parameter {
                    display_name,
                    lookup_name,
                    ty,
                    description,
                } => items.push(SectionItem::new(
                    SectionKind::Parameters,
                    Some(display_name.as_str()),
                    self.parameter_types
                        .type_for_value_field(lookup_name.as_str(), ty.as_deref()),
                    description,
                )),
                rst::Field::Attribute {
                    name,
                    ty,
                    description,
                } => items.push(SectionItem::new(
                    SectionKind::Attributes,
                    Some(name.as_str()),
                    self.attribute_types
                        .type_for_value_field(name.as_str(), ty.as_deref()),
                    description,
                )),
                rst::Field::Returns { name, description } => items.push(SectionItem::new(
                    SectionKind::Returns,
                    name.as_deref(),
                    self.return_type.filter(|ty| !ty.is_empty()),
                    description,
                )),
                rst::Field::Raises {
                    exception,
                    description,
                } => items.push(SectionItem::new(
                    SectionKind::Raises,
                    exception.as_deref(),
                    None,
                    description,
                )),
                rst::Field::ReturnType { .. } if !self.has_returns => {
                    if let Some(return_type) = self.return_type.filter(|ty| !ty.is_empty()) {
                        items.push(SectionItem::new(
                            SectionKind::Returns,
                            None,
                            Some(return_type),
                            "",
                        ));
                    }
                }
                rst::Field::ParameterType { .. }
                | rst::Field::AttributeType { .. }
                | rst::Field::ReturnType { .. }
                | rst::Field::Metadata
                | rst::Field::Unknown { .. } => {}
            }
        }

        items
    }
}

/// Tracks `:type name:` fields that supplement matching value fields.
///
/// A separate type field is usable only when the corresponding value field
/// exists and did not already include an inline type.
#[derive(Default)]
struct SupplementalTypeFields<'a> {
    types: FxHashMap<&'a str, &'a str>,
    value_fields_accepting_type: FxHashMap<&'a str, bool>,
}

impl<'a> SupplementalTypeFields<'a> {
    /// Returns the type to render for a value field.
    ///
    /// reST allows types inline on the value field:
    ///
    /// ```python
    /// """
    /// :param str value: The value.
    /// """
    /// ```
    ///
    /// It also allows types in separate supplemental fields:
    ///
    /// ```python
    /// """
    /// :param value: The value.
    /// :type value: str
    /// """
    /// ```
    ///
    /// Inline types win; supplemental types are only used for matching fields
    /// without inline types.
    fn type_for_value_field(&self, name: &str, inline_ty: Option<&'a str>) -> Option<&'a str> {
        inline_ty.or_else(|| self.get_non_empty(name))
    }

    fn record_value_field(&mut self, name: &'a str, has_inline_type: bool) {
        self.value_fields_accepting_type
            .entry(name)
            .and_modify(|accepts_separate_type| *accepts_separate_type &= !has_inline_type)
            .or_insert(!has_inline_type);
    }

    fn record_type_field(&mut self, name: &'a str, ty: &'a str) -> Option<()> {
        self.types.insert(name, ty).is_none().then_some(())
    }

    fn all_types_match_value_fields(&self) -> bool {
        self.types.keys().all(|name| {
            self.value_fields_accepting_type
                .get(name)
                .copied()
                .unwrap_or(false)
        })
    }

    fn get_non_empty(&self, name: &str) -> Option<&'a str> {
        self.types.get(name).copied().filter(|ty| !ty.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use insta::{Settings, assert_snapshot};

    use super::super::Docstring;
    use crate::docstring::formats::Formats;

    #[test]
    fn render_parameters_with_inline_and_supplemental_types() {
        let docstring = "\
Summary.

:param str value: The value.
:param other: Another value.
:type other: int
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        Summary.

        ## Parameters
        `value` (`str`): The value.
        `other` (`int`): Another value.
        ");
    }

    #[test]
    fn render_returns_with_supplemental_type() {
        let docstring = "\
Summary.

:returns: Whether validation passed.
:rtype: bool
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        Summary.

        ## Returns
        `bool`: Whether validation passed.
        ");
    }

    #[test]
    fn preserve_duplicate_parameters() {
        let docstring = "\
:param value: Stale description.
:param value: Corrected description.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Parameters
        `value`: Stale description.
        `value`: Corrected description.
        ");
    }

    #[test]
    fn render_parameter_and_standalone_return_type() {
        let docstring = "\
Summary.

:param value: The value.
:rtype: str
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        Summary.

        ## Parameters
        `value`: The value.

        ## Returns
        `str`
        ");
    }

    #[test]
    fn render_standalone_return_type() {
        let docstring = "\
Summary.

:rtype: str
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        Summary.

        ## Returns
        `str`
        ");
    }

    #[test]
    fn preserve_inline_roles_in_prose() {
        let docstring = "\
This is a function description.
:class:`Foo` instances can be passed here.

:param value: The value.
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        This is a function description.
        :class:`Foo` instances can be passed here.

        ## Parameters
        `value`: The value.
        ");
    }

    #[test]
    fn ignore_metadata_fields() {
        let docstring = "\
:param value: The value.
:meta private:
:returns: The result.
:meta hide-value:
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Parameters
        `value`: The value.

        ## Returns
        The result.
        ");
    }

    #[test]
    fn render_parameter_aliases_and_variadics() {
        let docstring = "\
:param param: The parameter description.
:type param: int
:kwparam retries: Retry attempts.
:paramtype retries: int
:param *args: Extra positional arguments.
:type args: tuple[str, ...]
:param **kwargs: Extra keyword arguments.
:type **kwargs: dict[str, object]
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Parameters
        `param` (`int`): The parameter description.
        `retries` (`int`): Retry attempts.
        `*args` (`tuple[str, ...]`): Extra positional arguments.
        `**kwargs` (`dict[str, object]`): Extra keyword arguments.
        ");
    }

    #[test]
    fn render_attribute_aliases() {
        let docstring = "\
:var cache: Cached data.
:vartype cache: dict[str,
    object]
:ivar state: Instance state.
:var str title: Display title.
:cvar VERSION: Package version.
:vartype VERSION: str
";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Attributes
        `cache` (`dict[str, object]`): Cached data.
        `state`: Instance state.
        `title` (`str`): Display title.
        `VERSION` (`str`): Package version.
        ");
    }

    #[test]
    fn render_named_returns_and_exceptions() {
        let docstring = "\
:returns baz: The return value description
:rtype: dict[str,
    int]
:raises ValueError: If the value is invalid.
:exception RuntimeError: If the system is unavailable.";
        let parsed = parse_docstring(docstring);

        assert_snapshot!(parsed.render_markdown(), @"
        ## Returns
        `baz` (`dict[str, int]`): The return value description

        ## Raises
        `ValueError`: If the value is invalid.
        `RuntimeError`: If the system is unavailable.
        ");
    }

    #[test]
    fn unstructured_field_lists_stay_raw() {
        for docstring in [
            // Recognized fields that would lose information or render no
            // visible item if converted structurally.
            // `:type orphan:` has no matching value field, so rendering the
            // field list structurally would drop the type information.
            "\
:param first: First parameter.
:type orphan: str
",
            // `:meta private:` is metadata only, so it does not produce any
            // visible Markdown section item.
            "\
:meta private:
",
            // Unsupported or ambiguous field lists.
            // `:unknown field:` is not a supported Sphinx-style field.
            "\
Summary.

:param value: The value.
:unknown field: Preserve this field list.
",
            // Empty `:returns:` and `:raises:` fields would render as empty
            // section items.
            "\
Summary.

:returns:
:raises:
",
            // `:type value:` cannot supplement a parameter that already has
            // an inline type.
            "\
Summary.

:param str value: The value.
:type value: int
",
            // Duplicate `:type value:` fields make the supplemental type
            // ambiguous.
            "\
Summary.

:param value: The value.
:type value: str
:type value: int
",
            // The empty `:returns:` field would render as an empty section item.
            "\
Summary.

:param value: The value.
:returns:
",
        ] {
            let parsed = parse_docstring(docstring);
            assert_eq!(parsed.render_markdown(), docstring);
        }
    }

    #[test]
    fn preserve_preformatted_field_lists() {
        let docstring = "\
Markdown input:

```text
:param sample: This is sample input
```

Doctest output:

>>> print(\"field list\")
:param sample: This is sample output

Literal block::

    :param sample: This is sample input

:param quoted: Example::

:param sample: This is sample input
:returns: This is still sample input

:param second:
    - First option.
    - Second option.
:param third:
    1. Validate the input.
    2. Return the result.
:param done: Whether work is done.";
        let parsed = parse_docstring(docstring);
        let mut settings = Settings::clone_current();
        settings.add_filter("\n    \n", "\n<INDENTED-BLANK>\n");
        let _snap = settings.bind_to_scope();

        assert_snapshot!(parsed.render_markdown(), @"
        Markdown input:

        ```text
        :param sample: This is sample input
        ```

        Doctest output:

        >>> print(\"field list\")
        :param sample: This is sample output

        Literal block::

            :param sample: This is sample input

        ## Parameters
        `quoted`: Example::
        <INDENTED-BLANK>
            :param sample: This is sample input
            :returns: This is still sample input
        `second`:

        - First option.
        - Second option.

        `third`:

        1. Validate the input.
        2. Return the result.

        `done`: Whether work is done.
        ");
    }

    #[test]
    fn field_lists_in_block_quotes_remain_raw() {
        let docstring = "\
Summary.

    :param value: The value.
    :returns: Another value.
";
        let parsed = parse_docstring(docstring);

        assert_eq!(parsed.render_markdown(), docstring);
    }

    fn parse_docstring(raw: &str) -> Docstring<'_> {
        let formats = Formats::parse(raw);
        Docstring::parse(raw, &formats)
    }
}
