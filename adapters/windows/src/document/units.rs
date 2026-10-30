// Copyright 2026 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

//! How a [`TextDocument`]'s units move: the starts of each unit worked out
//! once per document, and UI Automation's range operations on them.

use accesskit::TextDocumentRef;
use accesskit::text_document::{Annotation, LineBox, Look, Object, ParaKind};
use std::ops::Range;

/// A unit a screen reader moves by (UI Automation's `TextUnit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    Character,
    Format,
    Word,
    Line,
    Paragraph,
    Page,
    Document,
}

/// A value over a range: the same everywhere, or not.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Over<T> {
    Single(T),
    Mixed,
}

/// A snapshot with the unit boundaries worked out once.
pub(crate) struct Indexed {
    pub(crate) doc: TextDocumentRef,
    /// Starts of Format, Word, Line, Paragraph and Page units, each sorted, with 0 and without
    /// the document's end.
    bounds: [Vec<usize>; 5],
}

fn slot(unit: Unit) -> Option<usize> {
    match unit {
        Unit::Format => Some(0),
        Unit::Word => Some(1),
        Unit::Line => Some(2),
        Unit::Paragraph => Some(3),
        Unit::Page => Some(4),
        Unit::Character | Unit::Document => None,
    }
}

fn is_high(u: u16) -> bool {
    (0xD800..0xDC00).contains(&u)
}

fn is_low(u: u16) -> bool {
    (0xDC00..0xE000).contains(&u)
}

impl Indexed {
    pub(crate) fn new(snap: TextDocumentRef) -> Indexed {
        let len = snap.text.len();
        let paras: Vec<usize> = snap.paragraphs.iter().map(|p| p.0).collect();
        let tidy = |mut v: Vec<usize>| {
            v.push(0);
            v.retain(|p| *p < len || (len == 0 && *p == 0));
            v.sort_unstable();
            v.dedup();
            v
        };
        // A paragraph is always a new word, line and stretch of formatting; an error starts and
        // ends a stretch of formatting, so a screen reader reading by format hears it alone.
        let mut format: Vec<usize> = snap
            .looks
            .iter()
            .map(|l| l.0)
            .chain(paras.iter().copied())
            .collect();
        for (r, _) in &snap.annotations {
            format.push(r.start);
            format.push(r.end);
        }
        let words = snap.words.iter().chain(&paras).copied().collect();
        let lines = snap.lines.iter().chain(&paras).copied().collect();
        let bounds = [
            tidy(format),
            tidy(words),
            tidy(lines),
            tidy(paras.clone()),
            tidy(snap.pages.clone()),
        ];
        Indexed { doc: snap, bounds }
    }

    pub(crate) fn len(&self) -> usize {
        self.doc.text.len()
    }

    pub(crate) fn clamp(&self, r: (usize, usize)) -> (usize, usize) {
        let len = self.len();
        let (a, b) = (r.0.min(len), r.1.min(len));
        (a.min(b), b.max(a))
    }

    pub(crate) fn is_start(&self, pos: usize, unit: Unit) -> bool {
        match slot(unit) {
            Some(i) => self.bounds[i].binary_search(&pos).is_ok(),
            None if unit == Unit::Document => pos == 0,
            None => {
                !(pos > 0
                    && pos < self.len()
                    && is_low(self.doc.text[pos])
                    && is_high(self.doc.text[pos - 1]))
            }
        }
    }

    /// The last start of `unit` before `pos`, or 0.
    pub(crate) fn prev_start(&self, pos: usize, unit: Unit) -> usize {
        match slot(unit) {
            Some(i) => {
                let b = &self.bounds[i];
                let k = b.partition_point(|s| *s < pos);
                if k == 0 { 0 } else { b[k - 1] }
            }
            None if unit == Unit::Document => 0,
            None => {
                if pos == 0 {
                    0
                } else if pos >= 2
                    && is_low(self.doc.text[pos - 1])
                    && is_high(self.doc.text[pos - 2])
                {
                    pos - 2
                } else {
                    pos - 1
                }
            }
        }
    }

    /// The first start of `unit` after `pos`, or the document's end.
    pub(crate) fn next_start(&self, pos: usize, unit: Unit) -> usize {
        let len = self.len();
        if pos >= len {
            return len;
        }
        match slot(unit) {
            Some(i) => {
                let b = &self.bounds[i];
                let k = b.partition_point(|s| *s <= pos);
                b.get(k).copied().unwrap_or(len)
            }
            None if unit == Unit::Document => len,
            None => {
                if is_high(self.doc.text[pos]) && pos + 1 < len && is_low(self.doc.text[pos + 1]) {
                    pos + 2
                } else {
                    pos + 1
                }
            }
        }
    }

    /// The unit around the range's start (UIA's ExpandToEnclosingUnit).
    pub(crate) fn expand(&self, r: (usize, usize), unit: Unit) -> (usize, usize) {
        let (start, end) = self.clamp(r);
        if unit == Unit::Document {
            return (0, self.len());
        }
        if unit == Unit::Character && start == self.len() {
            // Windows screen readers expect nothing to happen at the document's end.
            return (start, end);
        }
        let start = if self.is_start(start, unit) {
            start
        } else {
            self.prev_start(start, unit)
        };
        if start == self.len() {
            (start, end.max(start))
        } else {
            (start, self.next_start(start, unit))
        }
    }

    fn step(&self, mut pos: usize, unit: Unit, count: i32) -> (usize, i32) {
        let mut moved = 0;
        for _ in 0..count.unsigned_abs() {
            let edge = if count > 0 {
                pos >= self.len()
            } else {
                pos == 0
            };
            if edge {
                break;
            }
            pos = if count > 0 {
                self.next_start(pos, unit)
            } else {
                self.prev_start(pos, unit)
            };
            moved += 1;
        }
        (pos, if count > 0 { moved } else { -moved })
    }

    /// Move the range by `count` units (UIA's Move): a degenerate range stays degenerate, any
    /// other becomes the whole unit it lands on.
    pub(crate) fn move_by(
        &self,
        r: (usize, usize),
        unit: Unit,
        count: i32,
    ) -> ((usize, usize), i32) {
        let (start, end) = self.clamp(r);
        let degenerate = start == end;
        let from = if degenerate || self.is_start(start, unit) {
            start
        } else {
            self.prev_start(start, unit)
        };
        let (to, moved) = self.step(from, unit, count);
        if moved == 0 {
            return ((start, end), 0);
        }
        let end = if degenerate || to == self.len() {
            to
        } else {
            self.next_start(to, unit)
        };
        ((to, end), moved)
    }

    /// Move one end of the range (UIA's MoveEndpointByUnit); the other follows if passed.
    pub(crate) fn move_end(
        &self,
        r: (usize, usize),
        at_end: bool,
        unit: Unit,
        count: i32,
    ) -> ((usize, usize), i32) {
        let (start, end) = self.clamp(r);
        let (pos, moved) = self.step(if at_end { end } else { start }, unit, count);
        (Self::set_end(start, end, at_end, pos), moved)
    }

    /// The range with one end set to `pos`; the other follows if passed.
    pub(crate) fn set_end(start: usize, end: usize, at_end: bool, pos: usize) -> (usize, usize) {
        if at_end {
            (start.min(pos), pos)
        } else {
            (pos, end.max(pos))
        }
    }

    pub(crate) fn text(&self, r: (usize, usize)) -> &[u16] {
        let (a, b) = self.clamp(r);
        &self.doc.text[a..b]
    }

    /// The value of `f` over the range: a degenerate range takes the character after it (the
    /// one before at the document's end).
    fn over<T: PartialEq, E>(
        &self,
        r: (usize, usize),
        items: &[(usize, E)],
        f: impl Fn(&E) -> T,
    ) -> Option<Over<T>> {
        let (start, end) = self.clamp(r);
        if items.is_empty() {
            return None;
        }
        let at = |pos: usize| items.partition_point(|i| i.0 <= pos).saturating_sub(1);
        let first = if start == end && start == self.len() && start > 0 {
            at(start - 1)
        } else {
            at(start)
        };
        let last = if end > start { at(end - 1) } else { first };
        let value = f(&items[first].1);
        for item in items.get(first + 1..=last).unwrap_or_default() {
            if f(&item.1) != value {
                return Some(Over::Mixed);
            }
        }
        Some(Over::Single(value))
    }

    pub(crate) fn look<T: PartialEq>(
        &self,
        r: (usize, usize),
        f: impl Fn(&Look) -> T,
    ) -> Option<Over<T>> {
        self.over(r, &self.doc.looks, |l| f(l))
    }

    pub(crate) fn para<T: PartialEq>(
        &self,
        r: (usize, usize),
        f: impl Fn(&ParaKind) -> T,
    ) -> Option<Over<T>> {
        self.over(r, &self.doc.paragraphs, |p| f(p))
    }

    /// The kinds of error anywhere in the range, as Word reports them (a paragraph with one
    /// misspelt word says SpellingError).
    pub(crate) fn annotations(&self, r: (usize, usize)) -> Vec<Annotation> {
        let (start, end) = self.clamp(r);
        let hit = |a: &Range<usize>| {
            if start == end {
                a.start <= start && start < a.end
            } else {
                a.start < end && start < a.end
            }
        };
        let mut kinds = Vec::new();
        for (a, kind) in &self.doc.annotations {
            if hit(a) && !kinds.contains(kind) {
                kinds.push(*kind);
            }
        }
        kinds
    }

    /// The elements at `path` (the document's own when empty).
    pub(crate) fn children(&self, path: &[usize]) -> &[Object] {
        match self.object(path) {
            Some(o) => &o.children,
            None if path.is_empty() => &self.doc.objects,
            None => &[],
        }
    }

    pub(crate) fn object(&self, path: &[usize]) -> Option<&Object> {
        let (first, rest) = path.split_first()?;
        let mut o = self.doc.objects.get(*first)?;
        for i in rest {
            o = o.children.get(*i)?;
        }
        Some(o)
    }

    /// The deepest element around the range (empty: the document).
    pub(crate) fn enclosing(&self, r: (usize, usize)) -> Vec<usize> {
        let (start, end) = self.clamp(r);
        let around = |o: &Object| {
            if start == end {
                o.range.start <= start && start < o.range.end
            } else {
                o.range.start <= start && end <= o.range.end
            }
        };
        let mut path = Vec::new();
        let mut level = &self.doc.objects;
        while let Some(i) = level.iter().position(&around) {
            path.push(i);
            level = &level[i].children;
        }
        path
    }

    /// The children of the range's enclosing element that the range touches.
    pub(crate) fn children_in(&self, r: (usize, usize)) -> Vec<Vec<usize>> {
        let (start, end) = self.clamp(r);
        if start == end {
            return Vec::new();
        }
        let parent = self.enclosing(r);
        self.children(&parent)
            .iter()
            .enumerate()
            .filter(|(_, o)| o.range.start < end && start < o.range.end)
            .map(|(i, _)| {
                let mut p = parent.clone();
                p.push(i);
                p
            })
            .collect()
    }

    /// The range's boxes, one per line it touches: (page, [left, top, right, bottom]) in page
    /// points. A degenerate range is a caret, one point wide.
    pub(crate) fn boxes(&self, r: (usize, usize)) -> Vec<(usize, [f32; 4])> {
        let (start, end) = self.clamp(r);
        let boxes = &self.doc.boxes;
        let x_at = |b: &LineBox, pos: usize| {
            let k = b.stops.partition_point(|s| s.0 <= pos);
            b.stops.get(k.saturating_sub(1)).map_or(0.0, |s| s.1)
        };
        let first = boxes
            .partition_point(|b| b.end <= start && b.start < start)
            .min(boxes.len().saturating_sub(1));
        let mut out = Vec::new();
        for b in boxes.get(first..).unwrap_or_default() {
            if b.start > end || (b.start == end && end > start) {
                break;
            }
            let x0 = x_at(b, start.max(b.start));
            let x1 = if start == end {
                x0 + 1.0
            } else {
                x_at(b, end.min(b.end))
            };
            out.push((b.page, [x0.min(x1), b.top, x0.max(x1), b.bottom]));
            if start == end {
                break;
            }
        }
        out
    }

    /// The text position nearest a point on page `page` (page points).
    pub(crate) fn hit(&self, page: usize, x: f32, y: f32) -> Option<usize> {
        let near = |b: &&LineBox| {
            if y < b.top {
                b.top - y
            } else if y > b.bottom {
                y - b.bottom
            } else {
                0.0
            }
        };
        let b = self
            .doc
            .boxes
            .iter()
            .filter(|b| b.page == page)
            .min_by(|a, b| near(a).total_cmp(&near(b)))?;
        let stop = b
            .stops
            .iter()
            .min_by(|a, c| (a.1 - x).abs().total_cmp(&(c.1 - x).abs()))?;
        Some(stop.0.min(b.end.saturating_sub(1)).max(b.start))
    }

    /// The text of the lines `visible` keeps, from the first one's start to the last one's end.
    pub(crate) fn span_of(&self, visible: impl Fn(&LineBox) -> bool) -> Option<(usize, usize)> {
        let mut it = self.doc.boxes.iter().filter(|b| visible(b));
        let first = it.next()?;
        let last = it.last().unwrap_or(first);
        Some((first.start, last.end))
    }

    /// The first (or last) place `needle` is found in the range.
    pub(crate) fn find(
        &self,
        r: (usize, usize),
        needle: &[u16],
        backward: bool,
        ignore_case: bool,
    ) -> Option<(usize, usize)> {
        let (start, end) = self.clamp(r);
        if needle.is_empty() || end - start < needle.len() {
            return None;
        }
        let fold = |u: u16| {
            if ignore_case {
                char::from_u32(u32::from(u)).map_or(u, |c| {
                    c.to_lowercase()
                        .next()
                        .map_or(u, |l| u16::try_from(u32::from(l)).unwrap_or(u))
                })
            } else {
                u
            }
        };
        let hay = &self.doc.text[start..end];
        let matches = |i: usize| {
            needle
                .iter()
                .enumerate()
                .all(|(k, n)| fold(hay[i + k]) == fold(*n))
        };
        let last = hay.len() - needle.len();
        let found = if backward {
            (0..=last).rev().find(|i| matches(*i))
        } else {
            (0..=last).find(|i| matches(*i))
        };
        found.map(|i| (start + i, start + i + needle.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accesskit::TextDocument;
    use accesskit::text_document::{Align, ObjectKind};

    fn doc() -> Indexed {
        // "Title\r" (heading), "One two\r", "x\u{1F600}y\r"
        let text: Vec<u16> = "Title\rOne two\rx\u{1F600}y\r".encode_utf16().collect();
        let kind = |id| ParaKind {
            style_id: id,
            style_name: String::new(),
            align: Align::Left,
            line_spacing: String::new(),
        };
        let look = |w| Look {
            font: "Aptos".into(),
            size: 12.0,
            weight: w,
            italic: false,
            underline: None,
            strike: None,
            superscript: false,
            subscript: false,
            color: None,
            background: None,
        };
        Indexed::new(TextDocumentRef::from(TextDocument {
            paragraphs: vec![(0, kind(70001)), (6, kind(70012)), (14, kind(70012))],
            lines: vec![0, 6, 14],
            pages: vec![0],
            words: vec![10],
            looks: vec![
                (0, look(700)),
                (6, look(400)),
                (10, look(700)),
                (13, look(400)),
            ],
            annotations: vec![(10..13, Annotation::Spelling)],
            objects: vec![Object {
                kind: ObjectKind::List,
                range: 6..19,
                children: vec![
                    Object {
                        kind: ObjectKind::ListItem,
                        range: 6..14,
                        children: vec![Object {
                            kind: ObjectKind::Link { url: "u".into() },
                            range: 10..13,
                            children: vec![],
                        }],
                    },
                    Object {
                        kind: ObjectKind::ListItem,
                        range: 14..19,
                        children: vec![],
                    },
                ],
            }],
            text,
            boxes: vec![],
        }))
    }

    #[test]
    fn elements_inside_the_text() {
        let d = doc();
        assert_eq!(d.enclosing((11, 11)), [0, 0, 0]);
        assert_eq!(d.enclosing((7, 13)), [0, 0]);
        assert_eq!(d.enclosing((2, 2)), Vec::<usize>::new());
        assert_eq!(d.children_in((0, 19)), [vec![0]]);
        assert_eq!(d.children_in((6, 19)), [vec![0, 0], vec![0, 1]]);
        assert_eq!(d.children_in((6, 14)), [vec![0, 0, 0]]);
        assert_eq!(d.object(&[0, 1]).map(|o| o.range.clone()), Some(14..19));
    }

    #[test]
    fn paragraphs_move_and_expand() {
        let d = doc();
        assert_eq!(d.len(), 19);
        assert_eq!(d.expand((8, 8), Unit::Paragraph), (6, 14));
        assert_eq!(d.move_by((0, 6), Unit::Paragraph, 1), ((6, 14), 1));
        assert_eq!(d.move_by((0, 6), Unit::Paragraph, 5), ((19, 19), 3));
        assert_eq!(d.move_by((14, 19), Unit::Paragraph, -2), ((0, 6), -2));
        // A degenerate range stays degenerate.
        assert_eq!(d.move_by((7, 7), Unit::Paragraph, 1), ((14, 14), 1));
    }

    #[test]
    fn words_break_at_paragraphs_and_stops() {
        let d = doc();
        assert_eq!(d.expand((7, 7), Unit::Word), (6, 10));
        assert_eq!(d.expand((11, 11), Unit::Word), (10, 14));
        assert_eq!(d.move_by((6, 10), Unit::Word, 1), ((10, 14), 1));
    }

    #[test]
    fn characters_keep_surrogate_pairs_whole() {
        let d = doc();
        assert_eq!(d.expand((15, 15), Unit::Character), (15, 17));
        assert_eq!(d.next_start(15, Unit::Character), 17);
        assert_eq!(d.prev_start(17, Unit::Character), 15);
        assert!(!d.is_start(16, Unit::Character));
        assert_eq!(d.expand((19, 19), Unit::Character), (19, 19));
    }

    #[test]
    fn endpoints_push_each_other() {
        let d = doc();
        assert_eq!(
            d.move_end((6, 10), false, Unit::Paragraph, 1),
            ((14, 14), 1)
        );
        assert_eq!(d.move_end((6, 10), true, Unit::Character, -6), ((4, 4), -6));
    }

    #[test]
    fn attributes_over_a_range() {
        let d = doc();
        assert_eq!(d.look((0, 5), |l| l.weight), Some(Over::Single(700)));
        assert_eq!(d.look((0, 8), |l| l.weight), Some(Over::Mixed));
        assert_eq!(d.look((19, 19), |l| l.weight), Some(Over::Single(400)));
        assert_eq!(d.para((2, 2), |p| p.style_id), Some(Over::Single(70001)));
        assert_eq!(d.para((0, 14), |p| p.style_id), Some(Over::Mixed));
        assert_eq!(d.annotations((10, 13)), vec![Annotation::Spelling]);
        assert_eq!(d.annotations((11, 11)), vec![Annotation::Spelling]);
        assert_eq!(d.annotations((6, 14)), vec![Annotation::Spelling]);
        assert_eq!(d.annotations((0, 5)), vec![]);
        // An error is a stretch of formatting of its own.
        assert_eq!(d.expand((11, 11), Unit::Format), (10, 13));
    }

    #[test]
    fn find_text() {
        let d = doc();
        let needle: Vec<u16> = "TWO".encode_utf16().collect();
        assert_eq!(d.find((0, 19), &needle, false, true), Some((10, 13)));
        assert_eq!(d.find((0, 19), &needle, false, false), None);
    }
}
