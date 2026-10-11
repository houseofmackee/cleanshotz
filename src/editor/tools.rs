//! Editor tools: names, toolbar icons and keyboard shortcuts.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Crop,
    Arrow,
    Line,
    Rectangle,
    Ellipse,
    Pen,
    Highlighter,
    Text,
    Counter,
    Pixelate,
    Blur,
    Spotlight,
    Magnify,
    Measure,
    Background,
}

impl Tool {
    pub const ALL: [Tool; 16] = [
        Tool::Select,
        Tool::Crop,
        Tool::Arrow,
        Tool::Line,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Pen,
        Tool::Highlighter,
        Tool::Text,
        Tool::Counter,
        Tool::Pixelate,
        Tool::Blur,
        Tool::Spotlight,
        Tool::Magnify,
        Tool::Measure,
        Tool::Background,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap()
    }

    pub fn title(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Crop => "Crop",
            Tool::Arrow => "Arrow",
            Tool::Line => "Line",
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Pen => "Pen",
            Tool::Highlighter => "Highlighter",
            Tool::Text => "Text",
            Tool::Counter => "Counter",
            Tool::Pixelate => "Pixelate",
            Tool::Blur => "Blur",
            Tool::Spotlight => "Spotlight",
            Tool::Magnify => "Magnify",
            Tool::Measure => "Measure",
            Tool::Background => "Background",
        }
    }

    /// SF Symbol for the toolbar.
    pub fn symbol(self) -> &'static str {
        match self {
            Tool::Select => "cursorarrow",
            Tool::Crop => "crop",
            Tool::Arrow => "arrow.up.right",
            Tool::Line => "line.diagonal",
            Tool::Rectangle => "rectangle",
            Tool::Ellipse => "circle",
            Tool::Pen => "scribble",
            Tool::Highlighter => "highlighter",
            Tool::Text => "textformat",
            Tool::Counter => "1.circle",
            Tool::Pixelate => "checkerboard.rectangle",
            Tool::Blur => "drop",
            Tool::Spotlight => "flashlight.on.fill",
            Tool::Magnify => "plus.magnifyingglass",
            Tool::Measure => "ruler",
            Tool::Background => "photo.on.rectangle",
        }
    }

    /// Single-key shortcut, used when no modifier is held.
    pub fn shortcut(self) -> char {
        match self {
            Tool::Select => 'v',
            Tool::Crop => 'c',
            Tool::Arrow => 'a',
            Tool::Line => 'l',
            Tool::Rectangle => 'r',
            Tool::Ellipse => 'o',
            Tool::Pen => 'p',
            Tool::Highlighter => 'h',
            Tool::Text => 't',
            Tool::Counter => 'n',
            Tool::Pixelate => 'x',
            Tool::Blur => 'b',
            Tool::Spotlight => 's',
            Tool::Magnify => 'm',
            Tool::Measure => 'u',
            Tool::Background => 'g',
        }
    }

    pub fn from_shortcut(c: char) -> Option<Tool> {
        let c = c.to_ascii_lowercase();
        Self::ALL.into_iter().find(|t| t.shortcut() == c)
    }

    /// Whether color and stroke width apply to this tool.
    pub fn uses_color(self) -> bool {
        !matches!(
            self,
            Tool::Select
                | Tool::Crop
                | Tool::Pixelate
                | Tool::Blur
                | Tool::Spotlight
                | Tool::Background
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_are_unique_and_round_trip() {
        for tool in Tool::ALL {
            assert_eq!(Tool::from_shortcut(tool.shortcut()), Some(tool));
            assert_eq!(Tool::ALL[tool.index()], tool);
        }
        assert_eq!(Tool::from_shortcut('A'), Some(Tool::Arrow));
        assert_eq!(Tool::from_shortcut('z'), None);
    }
}
