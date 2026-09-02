//! Turning collected errors into something a person can read.
//!
//! Compilation gathers every problem it can before giving up (`syntax::SyntaxError`),
//! so the unit of reporting is a *set* of problems against one file. `miette` renders
//! them with the source quoted and the spans marked.
//!
//! The same spans feed the `# src/foo.mwl:42` comments in debug output
//! (`docs/01-requirements.md` section 15), so there is one notion of "where" in the
//! compiler, not two.

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

use crate::syntax::SyntaxError;

/// Every problem found in one file.
#[derive(Debug, Error, Diagnostic)]
#[error("{}", self.summary())]
pub struct Report {
    path: String,
    #[related]
    pub problems: Vec<Problem>,
}

#[derive(Debug, Error, Diagnostic)]
#[error("{message}")]
pub struct Problem {
    message: String,
    #[source_code]
    src: NamedSource<String>,
    #[label("{message}")]
    span: SourceSpan,
}

impl Problem {
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Where it is, as a byte range into the source that was compiled.
    pub fn range(&self) -> (usize, usize) {
        (self.span.offset(), self.span.len())
    }
}

impl Report {
    fn summary(&self) -> String {
        match self.problems.len() {
            1 => format!("1 problem in {}", self.path),
            n => format!("{n} problems in {}", self.path),
        }
    }

    pub fn new(path: &str, src: &str, errors: Vec<SyntaxError>) -> Self {
        let problems = errors
            .into_iter()
            .map(|error| Problem {
                src: NamedSource::new(path, src.to_owned()).with_language("rust"),
                // A zero-width span at end of input is common — "expected '}'" after
                // an unterminated block — and must still point somewhere renderable.
                span: SourceSpan::new(
                    error.span.start.min(src.len()).into(),
                    error.span.end.saturating_sub(error.span.start),
                ),
                message: error.message,
            })
            .collect();
        Report {
            path: path.to_owned(),
            problems,
        }
    }

    /// `None` when there was nothing wrong, so callers can write
    /// `if let Some(report) = Report::of(..) { return Err(report) }`.
    pub fn of(path: &str, src: &str, errors: Vec<SyntaxError>) -> Option<Self> {
        if errors.is_empty() {
            None
        } else {
            Some(Report::new(path, src, errors))
        }
    }
}

// SPDX-License-Identifier: MIT

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{Span, SyntaxError};

    fn render(report: &Report) -> String {
        let mut out = String::new();
        miette::GraphicalReportHandler::new()
            .with_theme(miette::GraphicalTheme::unicode_nocolor())
            .render_report(&mut out, report)
            .expect("rendering a report cannot fail");
        out
    }

    #[test]
    fn one_error_becomes_one_labelled_problem() {
        let src = "fn main() { $ }";
        let errors = vec![SyntaxError::new(
            Span { start: 12, end: 13 },
            "unexpected character '$'",
        )];
        let report = Report::new("src/main.mwl", src, errors);
        assert_eq!(report.problems.len(), 1);

        let text = render(&report);
        assert!(text.contains("unexpected character '$'"), "{text}");
        assert!(text.contains("src/main.mwl"), "{text}");
        // The offending line is quoted back, with the span marked under it.
        assert!(text.contains("fn main() { $ }"), "{text}");
    }

    #[test]
    fn several_errors_are_reported_together() {
        let src = "fn a() { $ }\nfn b() { $ }";
        let errors = vec![
            SyntaxError::new(Span { start: 9, end: 10 }, "first"),
            SyntaxError::new(Span { start: 22, end: 23 }, "second"),
        ];
        let report = Report::new("x.mwl", src, errors);
        let text = render(&report);
        assert!(text.contains("first"), "{text}");
        assert!(text.contains("second"), "{text}");
        assert!(
            text.contains("2 problems"),
            "the summary counts them: {text}"
        );
    }

    #[test]
    fn a_span_at_the_end_of_input_still_renders() {
        // Parser errors at EOF carry a zero-width span there. It must not panic or be
        // clipped away, because "expected '}'" at EOF is a common diagnostic.
        let src = "fn main() {";
        let errors = vec![SyntaxError::new(
            Span {
                start: src.len(),
                end: src.len(),
            },
            "unterminated block: expected '}'",
        )];
        let text = render(&Report::new("x.mwl", src, errors));
        assert!(text.contains("expected '}'"), "{text}");
    }

    #[test]
    fn no_errors_means_no_report() {
        assert!(Report::of("x.mwl", "fn main() {}", Vec::new()).is_none());
    }

    #[test]
    fn a_report_is_an_error_in_its_own_right() {
        // So callers can `?` it out of a build function.
        fn build() -> Result<(), Report> {
            Err(Report::new(
                "x.mwl",
                "fn",
                vec![SyntaxError::new(Span { start: 0, end: 2 }, "boom")],
            ))
        }
        let err = build().unwrap_err();
        assert_eq!(err.to_string(), "1 problem in x.mwl");
    }
}
