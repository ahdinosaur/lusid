use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{Alignment, Line, TextStyle, View};

/// A block of [`Line`]s. The `Display` impl emits each line with a trailing
/// newline (via `writeln!`). As with [`Line`], `alignment` and `style` are
/// metadata for downstream renderers, not applied by `Display`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paragraph {
    pub lines: Vec<Line>,
    pub alignment: Option<Alignment>,
    pub style: TextStyle,
}

impl Paragraph {
    /// Create a new `Paragraph` from a list of lines.
    pub fn new<L: Into<Vec<Line>>>(lines: L) -> Self {
        Self {
            lines: lines.into(),
            alignment: None,
            style: TextStyle::default(),
        }
    }

    /// Create a styled `Paragraph` from lines.
    pub fn new_styled<L: Into<Vec<Line>>>(lines: L, style: TextStyle) -> Self {
        Self {
            lines: lines.into(),
            alignment: None,
            style,
        }
    }

    /// Set the alignment for the paragraph (builder style).
    pub fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = Some(alignment);
        self
    }

    /// Set the style for the entire paragraph (builder style).
    pub fn style(mut self, style: TextStyle) -> Self {
        self.style = style;
        self
    }

    /// Append a line to the paragraph (builder style).
    pub fn push_line(mut self, line: Line) -> Self {
        self.lines.push(line);
        self
    }
}

impl Display for Paragraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for line in self.lines.iter() {
            writeln!(f, "{line}")?
        }
        Ok(())
    }
}

impl From<Vec<Line>> for Paragraph {
    fn from(value: Vec<Line>) -> Self {
        Paragraph::new(value)
    }
}

impl From<Vec<&str>> for Paragraph {
    fn from(value: Vec<&str>) -> Self {
        let lines: Vec<Line> = value.into_iter().map(Line::from).collect();
        Paragraph::new(lines)
    }
}

impl From<Paragraph> for View {
    fn from(value: Paragraph) -> Self {
        View::Paragraph(value)
    }
}
