// Copyright 2026 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

//! A document described as text, for documents too long to describe node by
//! node (a word processor's): one node holds the whole text, where its units
//! start, how it looks, its errors, the elements in it (links, lists, tables,
//! pictures) and where each line sits on its page. Platform adapters compute
//! units and attributes from it on demand, the way word processors expose
//! their own documents (Microsoft Word's UI Automation text pattern).
//!
//! Positions are UTF-16 offsets into [`TextDocument::text`].

use alloc::{string::String, sync::Arc, vec::Vec};
use core::{fmt, ops::Deref, ops::Range};

use crate::Color;

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An underline or strikethrough.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum LineStyle {
    Single,
    Double,
    Dotted,
    Dashed,
    Wavy,
}

/// A paragraph's alignment as seen on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

/// How a stretch of text looks.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Look {
    pub font: String,
    /// Points.
    pub size: f32,
    pub weight: i32,
    pub italic: bool,
    pub underline: Option<LineStyle>,
    pub strike: Option<LineStyle>,
    pub superscript: bool,
    pub subscript: bool,
    /// The text's colour as it is drawn on screen (automatic colour resolved),
    /// if known.
    pub color: Option<Color>,
    /// The colour behind the text on screen: its highlight or shading, else
    /// the page's, if known.
    pub background: Option<Color>,
}

/// What a paragraph is.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ParaKind {
    /// UI Automation's StyleId: 70001..=70009 for Heading 1..9, 70010 Title,
    /// 70011 Subtitle, 70012 Normal, 70015 bulleted list, 70016 numbered
    /// list, 70000 any other style.
    pub style_id: i32,
    /// The style's name, as the application shows it.
    pub style_name: String,
    pub align: Align,
    /// The line spacing, as the application shows it (`1.15`, `At least
    /// 12 pt`); empty if not known.
    pub line_spacing: String,
}

/// A marked stretch of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Annotation {
    Spelling,
    Grammar,
}

/// What an element inside the text is.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ObjectKind {
    Link { url: String },
    List,
    ListItem,
    Table { rows: usize, cols: usize },
    Cell { row: usize, col: usize },
    Image { alt: String },
}

/// An element inside the text: screen readers find links, lists and tables
/// as elements, and read them through the text they cover.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Object {
    pub kind: ObjectKind,
    pub range: Range<usize>,
    pub children: Vec<Object>,
}

/// Where a laid-out line sits on its page, in page units (see
/// [`crate::Node::page_bounds`]).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LineBox {
    /// The line's text: `start..end`.
    pub start: usize,
    pub end: usize,
    pub page: usize,
    pub top: f32,
    pub bottom: f32,
    /// Where each character cluster starts (its offset and x), then the
    /// line's end and its x.
    pub stops: Vec<(usize, f32)>,
}

/// The document at one moment. Every list is sorted by position.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TextDocument {
    /// The whole document; every paragraph ends with `\r`.
    pub text: Vec<u16>,
    /// Where each paragraph starts and what it is; the first starts at 0.
    pub paragraphs: Vec<(usize, ParaKind)>,
    /// Where each laid-out line starts.
    pub lines: Vec<usize>,
    /// Where each page starts.
    pub pages: Vec<usize>,
    /// Where each word starts (where the application's next-word command
    /// stops).
    pub words: Vec<usize>,
    /// Where each stretch of one look starts, and the look (until the next).
    pub looks: Vec<(usize, Look)>,
    /// Spelling and grammar errors.
    pub annotations: Vec<(Range<usize>, Annotation)>,
    /// The elements inside the text, in reading order, each with the ones
    /// inside it.
    pub objects: Vec<Object>,
    /// Where each line sits, by start (empty if not known).
    pub boxes: Vec<LineBox>,
}

/// A shared [`TextDocument`], as a node property. Building one is the
/// application's work once per change; comparing two is cheap when they are
/// the same one.
#[derive(Clone, Default)]
pub struct TextDocumentRef(pub Arc<TextDocument>);

impl PartialEq for TextDocumentRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl fmt::Debug for TextDocumentRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TextDocument({} UTF-16 units, {} paragraphs)",
            self.0.text.len(),
            self.0.paragraphs.len()
        )
    }
}

impl Deref for TextDocumentRef {
    type Target = TextDocument;

    fn deref(&self) -> &TextDocument {
        &self.0
    }
}

impl From<Arc<TextDocument>> for TextDocumentRef {
    fn from(value: Arc<TextDocument>) -> Self {
        Self(value)
    }
}

impl From<TextDocument> for TextDocumentRef {
    fn from(value: TextDocument) -> Self {
        Self(Arc::new(value))
    }
}

#[cfg(feature = "serde")]
impl Serialize for TextDocumentRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for TextDocumentRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        TextDocument::deserialize(deserializer).map(|d| Self(Arc::new(d)))
    }
}
