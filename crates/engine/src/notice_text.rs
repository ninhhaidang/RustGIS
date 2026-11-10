//! Deferred interface text. The engine keeps English diagnostics; a front end translates
//! trusted templates while treating project names and other file contents as literal data.

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NoticeText {
    Plain(String),
    Format { key: &'static str, args: Vec<(&'static str, NoticeText)> },
}

impl From<String> for NoticeText {
    /// Keeps file contents literal rather than interpreting them as translation keys.
    fn from(text: String) -> Self {
        Self::Plain(text)
    }
}

impl NoticeText {
    /// A trusted source-language phrase, optionally with other phrases or literal data as args.
    pub(crate) fn format(key: &'static str, args: impl IntoIterator<Item = (&'static str, Self)>) -> Self {
        Self::Format { key, args: args.into_iter().collect() }
    }

    /// A trusted phrase without arguments (for example, the kind of item a repair changed).
    pub(crate) fn term(key: &'static str) -> Self {
        Self::format(key, [])
    }

    /// Joins independently translatable entries; list nesting is bounded by the repair kinds.
    pub(crate) fn list(items: impl IntoIterator<Item = Self>) -> Self {
        items
            .into_iter()
            .reduce(|first, next| Self::format("{first}, {next}", [("first", first), ("next", next)]))
            .unwrap_or_else(|| String::new().into())
    }

    /// Renders trusted templates through the front end's formatter; literal data bypasses it.
    pub(crate) fn render(&self, format: &impl Fn(&str, &[(&str, &str)]) -> String) -> String {
        match self {
            Self::Plain(text) => text.clone(),
            Self::Format { key, args } => {
                let rendered: Vec<_> = args.iter().map(|(name, value)| (*name, value.render(format))).collect();
                let args: Vec<_> = rendered.iter().map(|(name, value)| (*name, value.as_str())).collect();
                format(key, &args)
            }
        }
    }

    /// The engine, CLI, Python and copied diagnostics retain source-language text.
    pub(crate) fn english(&self) -> String {
        self.render(&|key, args| {
            let mut output = String::new();
            let mut rest = key;
            while let Some((before, after)) = rest.split_once('{') {
                output.push_str(before);
                let Some((name, tail)) = after.split_once('}') else {
                    output.push('{');
                    output.push_str(after);
                    return output;
                };
                if let Some((_, value)) = args.iter().find(|(argument, _)| *argument == name) {
                    output.push_str(value);
                } else {
                    output.push('{');
                    output.push_str(name);
                    output.push('}');
                }
                rest = tail;
            }
            output.push_str(rest);
            output
        })
    }

    /// Redacts data before retaining translation arguments, as the ordinary notice path does.
    pub(crate) fn redacted(self) -> Self {
        match self {
            Self::Plain(text) => Self::Plain(crate::Session::redacted(&text).into_string()),
            Self::Format { key, args } => {
                Self::Format { key, args: args.into_iter().map(|(name, value)| (name, value.redacted())).collect() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translation_arguments_are_redacted_before_the_notice_retains_them() -> Result<(), String> {
        let mut session = crate::Session::new(crate::Project::default());
        let message =
            NoticeText::format("Removed {name}", [("name", "https://example.invalid/data?token=NOT_A_REAL_NOTICE_KEY".to_string().into())]);
        session.notify_text(message, false);
        let notice = session.notices.last().ok_or("missing notice")?;
        assert!(!format!("{notice:?}").contains("NOT_A_REAL_NOTICE_KEY"));
        let translated = notice
            .translated(|key, args| args.iter().fold(key.to_string(), |text, (name, value)| text.replace(&format!("{{{name}}}"), value)))
            .ok_or("missing translation")?;
        assert!(!translated.contains("NOT_A_REAL_NOTICE_KEY"));
        Ok(())
    }

    #[test]
    fn inserted_names_are_never_read_as_more_placeholders() {
        let text =
            NoticeText::format("{name}: {message}", [("name", "{message}".to_string().into()), ("message", "removed".to_string().into())]);
        assert_eq!(text.english(), "{message}: removed");
    }
}
