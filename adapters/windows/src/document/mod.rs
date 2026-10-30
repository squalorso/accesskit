// Copyright 2026 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

//! A node with a [`TextDocument`](accesskit::TextDocument): one text pattern
//! over the whole text, the way word processors expose their documents, with
//! the links, lists, tables and pictures in it as elements under the node.
//! Units and attributes are computed on demand from the document.

#![allow(non_upper_case_globals, non_snake_case)]

mod units;

use accesskit::text_document::{Align, Annotation, LineStyle, ObjectKind};
use accesskit::{
    Action, ActionData, ActionRequest, Affine, Color, Point, TextPosition, TextSelection,
};
use accesskit_consumer::{FullNodeId, NodeRef};
use std::mem::ManuallyDrop;
use std::sync::{Arc, RwLock, Weak};
use windows::{
    Win32::{
        System::{Com::*, Ole::*, Variant::*},
        UI::Accessibility::*,
    },
    core::*,
};

use crate::{context::Context, node::runtime_id_from_node_id, util::*};
pub(crate) use units::Indexed;
use units::{Over, Unit};

/// A log of the calls screen readers make on documents, to see how they read
/// them: build with the `document-trace` feature and set `ACCESSKIT_DOC_LOG`
/// to a file.
pub(crate) fn trace(args: std::fmt::Arguments) {
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    static FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    let file = FILE.get_or_init(|| {
        let path = std::env::var_os("ACCESSKIT_DOC_LOG")?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(Mutex::new)
    });
    if let Some(file) = file {
        let ms = START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis();
        if let Ok(mut f) = file.lock() {
            let _ = writeln!(f, "{ms:>8} {args}");
        }
    }
}

macro_rules! t {
    ($($a:tt)*) => {
        if cfg!(feature = "document-trace") {
            trace(format_args!($($a)*))
        }
    };
}

/// A range for the log: its ends and the start of its text.
fn show(doc: &Indexed, s: (usize, usize)) -> String {
    let text: String = String::from_utf16_lossy(doc.text(s))
        .chars()
        .take(24)
        .collect();
    format!("{}..{} {:?}", s.0, s.1, text)
}

/// The second number of our elements' runtime ids ("TD", text document),
/// after `UiaAppendRuntimeId`; the document node's id follows.
const OURS: i32 = 0x5444;

/// A document node, as its providers know it.
#[derive(Clone)]
pub(crate) struct Doc {
    context: Weak<Context>,
    node: FullNodeId,
}

/// The document's state for one call.
struct Now {
    doc: Arc<Indexed>,
    /// Anchor and focus.
    selection: (usize, usize),
    focused: bool,
    /// Each page's rectangle in the node's coordinates and its scale.
    pages: Vec<([f64; 4], f64)>,
    /// The visible part, in the node's coordinates.
    area: Option<[f64; 4]>,
    /// From the node's coordinates to the window's client area.
    transform: Affine,
    client: Point,
}

impl Doc {
    pub(crate) fn new(context: Weak<Context>, node: FullNodeId) -> Self {
        Self { context, node }
    }

    fn context(&self) -> Result<Arc<Context>> {
        upgrade(&self.context)
    }

    fn now(&self) -> Result<Now> {
        let context = self.context()?;
        let tree = context.read_tree();
        let node = tree
            .state()
            .node_by_id(self.node)
            .ok_or_else(element_not_available)?;
        let data = node.data();
        let current = data.text_document().ok_or_else(element_not_available)?;
        let doc = context.indexed(self.node, current);
        let local = tree.state().locate_node(self.node).map(|l| l.0);
        let selection = data
            .text_selection()
            .filter(|s| Some(s.anchor.node) == local && Some(s.focus.node) == local)
            .map_or((0, 0), |s| {
                (s.anchor.character_index, s.focus.character_index)
            });
        let pages = data
            .page_bounds()
            .unwrap_or_default()
            .chunks_exact(5)
            .map(|c| {
                (
                    [
                        f64::from(c[0]),
                        f64::from(c[1]),
                        f64::from(c[2]),
                        f64::from(c[3]),
                    ],
                    f64::from(c[4]),
                )
            })
            .collect();
        let area = data.bounds().map(|r| [r.x0, r.y0, r.x1, r.y1]);
        Ok(Now {
            doc,
            selection,
            focused: node.is_focused(),
            pages,
            area,
            transform: node.transform(),
            client: context.client_top_left(),
        })
    }

    fn doc(&self) -> Result<Arc<Indexed>> {
        Ok(self.now()?.doc)
    }

    fn selection(&self) -> (usize, usize) {
        self.now().map_or((0, 0), |n| n.selection)
    }

    fn node_ref_runtime_id(&self) -> [i32; 5] {
        runtime_id_from_node_id(self.node)
    }

    fn platform_node(&self) -> Result<IRawElementProviderFragment> {
        Ok(self
            .context()?
            .get_or_create_platform_node(self.node)
            .into_interface())
    }

    fn root(&self) -> Result<IRawElementProviderFragmentRoot> {
        let context = self.context()?;
        let root = context.read_tree().state().root_id();
        Ok(context.get_or_create_platform_node(root).into_interface())
    }

    /// Ask the application to select a range (a screen reader moving the caret).
    fn select(&self, span: (usize, usize)) -> Result<()> {
        let context = self.context()?;
        let tree = context.read_tree();
        let (local, tree_id) = tree
            .state()
            .locate_node(self.node)
            .ok_or_else(element_not_available)?;
        drop(tree);
        let at = |i| TextPosition {
            node: local,
            character_index: i,
        };
        context.do_action(ActionRequest {
            action: Action::SetTextSelection,
            target_tree: tree_id,
            target_node: local,
            data: Some(ActionData::SetTextSelection(TextSelection {
                anchor: at(span.0),
                focus: at(span.1),
            })),
        });
        Ok(())
    }
}

impl Now {
    /// A box in page units, on screen: left, top, width, height.
    fn on_screen(&self, page: usize, r: [f32; 4]) -> Option<[f64; 4]> {
        let (rect, s) = self.pages.get(page)?;
        let at = |x: f32, y: f32| {
            let p =
                self.transform * Point::new(rect[0] + f64::from(x) * s, rect[1] + f64::from(y) * s);
            (p.x + self.client.x, p.y + self.client.y)
        };
        let (l, t) = at(r[0], r[1]);
        let (rr, b) = at(r[2], r[3]);
        Some([l, t, rr - l, b - t])
    }

    fn rects(&self, span: (usize, usize)) -> Vec<[f64; 4]> {
        self.doc
            .boxes(span)
            .into_iter()
            .filter_map(|(page, r)| self.on_screen(page, r))
            .collect()
    }

    /// The part of the document on screen (everything when not known).
    fn visible(&self) -> (usize, usize) {
        let Some(area) = self.area.filter(|_| !self.pages.is_empty()) else {
            return (0, self.doc.len());
        };
        let shown = |b: &accesskit::text_document::LineBox| {
            self.pages.get(b.page).is_some_and(|(r, s)| {
                let (top, bottom) = (r[1] + f64::from(b.top) * s, r[1] + f64::from(b.bottom) * s);
                bottom > area[1] && top < area[3]
            })
        };
        self.doc.span_of(shown).unwrap_or((0, 0))
    }

    /// The text position under a point on screen.
    fn at_point(&self, x: f64, y: f64) -> Option<usize> {
        let p = self.transform.inverse() * Point::new(x - self.client.x, y - self.client.y);
        let dist = |r: &[f64; 4]| (r[1] - p.y).max(p.y - r[3]).max(0.0);
        let (i, (r, s)) = self
            .pages
            .iter()
            .enumerate()
            .min_by(|a, b| dist(&a.1.0).total_cmp(&dist(&b.1.0)))?;
        self.doc
            .hit(i, ((p.x - r[0]) / s) as f32, ((p.y - r[1]) / s) as f32)
    }
}

fn unit(u: TextUnit) -> Result<Unit> {
    Ok(match u {
        TextUnit_Character => Unit::Character,
        TextUnit_Format => Unit::Format,
        TextUnit_Word => Unit::Word,
        TextUnit_Line => Unit::Line,
        TextUnit_Paragraph => Unit::Paragraph,
        TextUnit_Page => Unit::Page,
        TextUnit_Document => Unit::Document,
        _ => return Err(invalid_arg()),
    })
}

fn at_end(e: TextPatternRangeEndpoint) -> Result<bool> {
    match e {
        TextPatternRangeEndpoint_Start => Ok(false),
        TextPatternRangeEndpoint_End => Ok(true),
        _ => Err(invalid_arg()),
    }
}

fn reserved_not_supported() -> Result<VARIANT> {
    Ok(unsafe { UiaGetReservedNotSupportedValue() }?.into())
}

fn mixed() -> Result<VARIANT> {
    Ok(unsafe { UiaGetReservedMixedAttributeValue() }?.into())
}

fn over<T>(v: Option<Over<T>>, f: impl Fn(T) -> VARIANT) -> Result<VARIANT> {
    match v {
        Some(Over::Single(v)) => Ok(f(v)),
        Some(Over::Mixed) => mixed(),
        None => reserved_not_supported(),
    }
}

/// An attribute the application may leave unknown: not supported where it
/// does.
fn known<T: Into<VARIANT>>(v: Option<Over<Option<T>>>) -> Result<VARIANT> {
    match v {
        Some(Over::Single(Some(v))) => Ok(v.into()),
        Some(Over::Mixed) => mixed(),
        _ => reserved_not_supported(),
    }
}

/// A colour as UI Automation gives it: a COLORREF, 0x00BBGGRR.
fn colorref(c: Color) -> i32 {
    i32::from(c.red) | (i32::from(c.green) << 8) | (i32::from(c.blue) << 16)
}

fn line_style(s: Option<LineStyle>) -> i32 {
    match s {
        None => TextDecorationLineStyle_None.0,
        Some(LineStyle::Single) => TextDecorationLineStyle_Single.0,
        Some(LineStyle::Double) => TextDecorationLineStyle_Double.0,
        Some(LineStyle::Dotted) => TextDecorationLineStyle_Dot.0,
        Some(LineStyle::Dashed) => TextDecorationLineStyle_Dash.0,
        Some(LineStyle::Wavy) => TextDecorationLineStyle_Wavy.0,
    }
}

fn i32_array_variant(items: &[i32]) -> VARIANT {
    let parray = safe_array_from_i32_slice(items);
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VARENUM(VT_ARRAY.0 | VT_I4.0),
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { parray },
            }),
        },
    }
}

fn rect_array(rects: &[[f64; 4]]) -> *mut SAFEARRAY {
    let flat: Vec<f64> = rects.iter().flatten().copied().collect();
    safe_array_from_f64_slice(&flat)
}

/// The rectangle around boxes (left, top, width, height).
fn uia_rect(rects: &[[f64; 4]]) -> UiaRect {
    let mut it = rects.iter();
    let Some(first) = it.next() else {
        return UiaRect::default();
    };
    let (mut l, mut t, mut r, mut b) =
        (first[0], first[1], first[0] + first[2], first[1] + first[3]);
    for x in it {
        l = l.min(x[0]);
        t = t.min(x[1]);
        r = r.max(x[0] + x[2]);
        b = b.max(x[1] + x[3]);
    }
    UiaRect {
        left: l,
        top: t,
        width: r - l,
        height: b - t,
    }
}

/// The runtime id of an element provider, read safely through its interface.
fn runtime_id(child: &IRawElementProviderSimple) -> Option<Vec<i32>> {
    let sa = unsafe {
        child
            .cast::<IRawElementProviderFragment>()
            .ok()?
            .GetRuntimeId()
    }
    .ok()?;
    if sa.is_null() {
        return None;
    }
    unsafe {
        let (lo, hi) = (
            SafeArrayGetLBound(sa, 1).ok()?,
            SafeArrayGetUBound(sa, 1).ok()?,
        );
        let ids = (lo..=hi)
            .map(|i| {
                let mut x = 0i32;
                SafeArrayGetElement(sa, &i, (&mut x as *mut i32).cast()).map(|_| x)
            })
            .collect::<Result<Vec<i32>>>();
        let _ = SafeArrayDestroy(sa);
        ids.ok()
    }
}

/// Whether a text range is one of ours: only ours implement
/// `ITextRangeProvider2`, so the two kinds of range in this process are told
/// apart without guessing.
pub(crate) fn is_document_range(range: &ITextRangeProvider) -> bool {
    range.cast::<ITextRangeProvider2>().is_ok()
}

/// A text attribute's value over a range (UIA's GetAttributeValue).
fn attribute(doc: &Indexed, id: UIA_TEXTATTRIBUTE_ID, r: (usize, usize)) -> Result<VARIANT> {
    match id {
        UIA_FontNameAttributeId => over(doc.look(r, |l| l.font.clone()), |f| {
            BSTR::from(f.as_str()).into()
        }),
        UIA_FontSizeAttributeId => over(doc.look(r, |l| l.size), |s| f64::from(s).into()),
        UIA_FontWeightAttributeId => over(doc.look(r, |l| l.weight), |w| w.into()),
        UIA_IsItalicAttributeId => over(doc.look(r, |l| l.italic), |i| i.into()),
        UIA_UnderlineStyleAttributeId => {
            over(doc.look(r, |l| line_style(l.underline)), |s| s.into())
        }
        UIA_StrikethroughStyleAttributeId => {
            over(doc.look(r, |l| line_style(l.strike)), |s| s.into())
        }
        UIA_IsSuperscriptAttributeId => over(doc.look(r, |l| l.superscript), |b| b.into()),
        UIA_IsSubscriptAttributeId => over(doc.look(r, |l| l.subscript), |b| b.into()),
        UIA_ForegroundColorAttributeId => known(doc.look(r, |l| l.color.map(colorref))),
        UIA_BackgroundColorAttributeId => known(doc.look(r, |l| l.background.map(colorref))),
        UIA_StyleIdAttributeId => over(doc.para(r, |p| p.style_id), |s| s.into()),
        UIA_StyleNameAttributeId => over(doc.para(r, |p| p.style_name.clone()), |n| {
            BSTR::from(n.as_str()).into()
        }),
        UIA_HorizontalTextAlignmentAttributeId => over(
            doc.para(r, |p| match p.align {
                Align::Left => HorizontalTextAlignment_Left.0,
                Align::Center => HorizontalTextAlignment_Centered.0,
                Align::Right => HorizontalTextAlignment_Right.0,
                Align::Justify => HorizontalTextAlignment_Justified.0,
            }),
            |a| a.into(),
        ),
        UIA_LineSpacingAttributeId => known(doc.para(r, |p| {
            (!p.line_spacing.is_empty()).then(|| BSTR::from(p.line_spacing.as_str()))
        })),
        UIA_AnnotationTypesAttributeId => {
            let ids: Vec<i32> = doc
                .annotations(r)
                .iter()
                .map(|k| match k {
                    Annotation::Spelling => AnnotationType_SpellingError.0,
                    Annotation::Grammar => AnnotationType_GrammarError.0,
                })
                .collect();
            Ok(i32_array_variant(&ids))
        }
        UIA_IsReadOnlyAttributeId | UIA_IsHiddenAttributeId => Ok(false.into()),
        UIA_CaretPositionAttributeId => Ok(CaretPosition_Unknown.0.into()),
        _ => reserved_not_supported(),
    }
}

/// Whether a range's attribute has the value a client looks for (UIA's
/// FindAttribute): numbers, flags and names; for annotation types, whether the
/// range has that annotation.
fn has_value(doc: &Indexed, id: UIA_TEXTATTRIBUTE_ID, r: (usize, usize), value: &VARIANT) -> bool {
    if id == UIA_AnnotationTypesAttributeId {
        let Ok(wanted) = i32::try_from(value) else {
            return false;
        };
        return doc.annotations(r).iter().any(|k| match k {
            Annotation::Spelling => AnnotationType_SpellingError.0 == wanted,
            Annotation::Grammar => AnnotationType_GrammarError.0 == wanted,
        });
    }
    let Ok(ours) = attribute(doc, id, r) else {
        return false;
    };
    if let (Ok(a), Ok(b)) = (i32::try_from(&ours), i32::try_from(value)) {
        return a == b;
    }
    if let (Ok(a), Ok(b)) = (bool::try_from(&ours), bool::try_from(value)) {
        return a == b;
    }
    if let (Ok(a), Ok(b)) = (f64::try_from(&ours), f64::try_from(value)) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (text_of(&ours), text_of(value)) {
        return a == b;
    }
    false
}

/// A string variant's text.
fn text_of(v: &VARIANT) -> Option<String> {
    // SAFETY: the union is read as a BSTR only when its type says it is one.
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        (inner.vt == VT_BSTR).then(|| inner.Anonymous.bstrVal.to_string())
    }
}

/// The unit a text attribute changes by: paragraphs for a paragraph's
/// attributes, stretches of formatting for the others.
fn attribute_unit(id: UIA_TEXTATTRIBUTE_ID) -> Unit {
    match id {
        UIA_StyleIdAttributeId
        | UIA_StyleNameAttributeId
        | UIA_HorizontalTextAlignmentAttributeId
        | UIA_LineSpacingAttributeId => Unit::Paragraph,
        _ => Unit::Format,
    }
}

/// The first (or last) stretch of `r` where the attribute has the value, with
/// the units next to it that have it too.
fn find_attribute(
    doc: &Indexed,
    id: UIA_TEXTATTRIBUTE_ID,
    value: &VARIANT,
    r: (usize, usize),
    backward: bool,
) -> Option<(usize, usize)> {
    let (start, end) = doc.clamp(r);
    let unit = attribute_unit(id);
    let at = |s: usize, e: usize| has_value(doc, id, (s, e), value);
    if !backward {
        let mut pos = start;
        while pos < end {
            let next = doc.next_start(pos, unit).min(end).max(pos + 1);
            if at(pos, next) {
                let mut e = next;
                while e < end {
                    let n = doc.next_start(e, unit).min(end).max(e + 1);
                    if !at(e, n) {
                        break;
                    }
                    e = n;
                }
                return Some((pos, e));
            }
            pos = next;
        }
    } else {
        let mut pos = end;
        while pos > start {
            let prev = doc.prev_start(pos, unit).max(start).min(pos - 1);
            if at(prev, pos) {
                let mut s = prev;
                while s > start {
                    let p = doc.prev_start(s, unit).max(start).min(s - 1);
                    if !at(p, s) {
                        break;
                    }
                    s = p;
                }
                return Some((s, pos));
            }
            pos = prev;
        }
    }
    None
}

/// The document's text pattern.
#[implement(ITextProvider, ITextProvider2)]
pub(crate) struct DocText {
    doc: Doc,
}

impl DocText {
    pub(crate) fn new(doc: Doc) -> Self {
        Self { doc }
    }
}

fn range(doc: &Doc, span: (usize, usize)) -> ITextRangeProvider {
    DocRange {
        doc: doc.clone(),
        span: RwLock::new(span),
    }
    .into()
}

impl ITextProvider_Impl for DocText_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let (a, f) = self.doc.selection();
        t!("GetSelection -> {a}..{f}");
        let r: IUnknown = range(&self.doc, (a.min(f), a.max(f))).cast()?;
        Ok(safe_array_from_com_slice(&[r]))
    }

    fn GetVisibleRanges(&self) -> Result<*mut SAFEARRAY> {
        let r: IUnknown = range(&self.doc, self.doc.now()?.visible()).cast()?;
        Ok(safe_array_from_com_slice(&[r]))
    }

    fn RangeFromChild(&self, child: Ref<IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        let child = required_param(&child)?;
        let doc = self.doc.doc()?;
        let id = runtime_id(child).ok_or_else(invalid_arg)?;
        let own = self.doc.node_ref_runtime_id();
        // The document itself (NVDA asks, from the enclosing element of a
        // position at the top level).
        if id.as_slice() == own.as_slice() {
            t!("RangeFromChild(document)");
            return Ok(range(&self.doc, (0, doc.len())));
        }
        t!("RangeFromChild({:?})", path_of(&own, &id));
        match path_of(&own, &id).and_then(|p| doc.object(&p).map(|o| (o.range.start, o.range.end)))
        {
            Some(span) => Ok(range(&self.doc, span)),
            None => Err(invalid_arg()),
        }
    }

    fn RangeFromPoint(&self, point: &UiaPoint) -> Result<ITextRangeProvider> {
        let now = self.doc.now()?;
        let at = now.at_point(point.x, point.y).unwrap_or(now.selection.1);
        t!("RangeFromPoint({}, {}) -> {at}", point.x, point.y);
        Ok(range(&self.doc, (at, at)))
    }

    fn DocumentRange(&self) -> Result<ITextRangeProvider> {
        Ok(range(&self.doc, (0, self.doc.doc()?.len())))
    }

    fn SupportedTextSelection(&self) -> Result<SupportedTextSelection> {
        Ok(SupportedTextSelection_Single)
    }
}

impl ITextProvider2_Impl for DocText_Impl {
    fn RangeFromAnnotation(
        &self,
        _annotation: Ref<IRawElementProviderSimple>,
    ) -> Result<ITextRangeProvider> {
        Err(invalid_arg())
    }

    fn GetCaretRange(&self, isactive: *mut BOOL) -> Result<ITextRangeProvider> {
        let now = self.doc.now()?;
        if !isactive.is_null() {
            unsafe { *isactive = now.focused.into() };
        }
        let f = now.selection.1;
        t!("GetCaretRange -> {f}");
        Ok(range(&self.doc, (f, f)))
    }
}

/// The text pattern of an element inside the text (a table cell): its part of
/// the document's text. Screen readers moving between cells select its range
/// (JAWS asks the target cell for one; without it, it moves by a line).
#[implement(ITextProvider)]
pub(crate) struct PartText {
    doc: Doc,
    span: (usize, usize),
}

impl ITextProvider_Impl for PartText_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let (a, f) = self.doc.selection();
        t!("Part {:?} GetSelection -> {a}..{f}", self.span);
        let r: IUnknown = range(&self.doc, (a.min(f), a.max(f))).cast()?;
        Ok(safe_array_from_com_slice(&[r]))
    }

    fn GetVisibleRanges(&self) -> Result<*mut SAFEARRAY> {
        let r: IUnknown = range(&self.doc, self.span).cast()?;
        Ok(safe_array_from_com_slice(&[r]))
    }

    fn RangeFromChild(&self, _child: Ref<IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        Err(invalid_arg())
    }

    fn RangeFromPoint(&self, point: &UiaPoint) -> Result<ITextRangeProvider> {
        let now = self.doc.now()?;
        let at = now
            .at_point(point.x, point.y)
            .unwrap_or(self.span.0)
            .clamp(self.span.0, self.span.1);
        Ok(range(&self.doc, (at, at)))
    }

    fn DocumentRange(&self) -> Result<ITextRangeProvider> {
        t!("Part {:?} DocumentRange", self.span);
        Ok(range(&self.doc, self.span))
    }

    fn SupportedTextSelection(&self) -> Result<SupportedTextSelection> {
        Ok(SupportedTextSelection_Single)
    }
}

/// A stretch of the document's text.
#[implement(ITextRangeProvider, ITextRangeProvider2)]
pub(crate) struct DocRange {
    doc: Doc,
    span: RwLock<(usize, usize)>,
}

impl DocRange {
    fn span(&self) -> (usize, usize) {
        self.span.read().map(|s| *s).unwrap_or((0, 0))
    }

    fn set(&self, s: (usize, usize)) {
        if let Ok(mut span) = self.span.write() {
            *span = s;
        }
    }

    /// The other range of a call: one of ours, of the same document.
    fn other<'a>(&self, r: &'a Ref<ITextRangeProvider>) -> Result<&'a DocRange> {
        let r = required_param(r)?;
        if !is_document_range(r) {
            return Err(invalid_arg());
        }
        let other: &DocRange = unsafe { r.as_impl() };
        if other.doc.node == self.doc.node && other.doc.context.ptr_eq(&self.doc.context) {
            Ok(other)
        } else {
            Err(invalid_arg())
        }
    }
}

impl ITextRangeProvider_Impl for DocRange_Impl {
    fn Clone(&self) -> Result<ITextRangeProvider> {
        Ok(range(&self.doc, self.span()))
    }

    fn Compare(&self, other: Ref<ITextRangeProvider>) -> Result<BOOL> {
        Ok(self
            .other(&other)
            .is_ok_and(|o| o.span() == self.span())
            .into())
    }

    fn CompareEndpoints(
        &self,
        endpoint: TextPatternRangeEndpoint,
        other: Ref<ITextRangeProvider>,
        other_endpoint: TextPatternRangeEndpoint,
    ) -> Result<i32> {
        let other = self.other(&other)?;
        let pick = |s: (usize, usize), e: bool| if e { s.1 } else { s.0 };
        let a = pick(self.span(), at_end(endpoint)?);
        let b = pick(other.span(), at_end(other_endpoint)?);
        Ok(a.cmp(&b) as i32)
    }

    fn ExpandToEnclosingUnit(&self, u: TextUnit) -> Result<()> {
        let doc = self.doc.doc()?;
        let before = self.span();
        self.set(doc.expand(before, unit(u)?));
        t!(
            "Expand({:?}) {} -> {}",
            unit(u)?,
            show(&doc, before),
            show(&doc, self.span())
        );
        Ok(())
    }

    fn FindAttribute(
        &self,
        id: UIA_TEXTATTRIBUTE_ID,
        value: &VARIANT,
        backward: BOOL,
    ) -> Result<ITextRangeProvider> {
        let doc = self.doc.doc()?;
        let found = find_attribute(&doc, id, value, self.span(), backward.as_bool());
        t!(
            "FindAttribute({}, backward={}) {:?} -> {:?}",
            id.0,
            backward.as_bool(),
            self.span(),
            found
        );
        match found {
            Some(span) => Ok(range(&self.doc, span)),
            None => Err(Error::empty()),
        }
    }

    fn FindText(
        &self,
        text: &BSTR,
        backward: BOOL,
        ignore_case: BOOL,
    ) -> Result<ITextRangeProvider> {
        let doc = self.doc.doc()?;
        match doc.find(self.span(), text, backward.as_bool(), ignore_case.as_bool()) {
            Some(s) => Ok(range(&self.doc, s)),
            None => Err(Error::empty()),
        }
    }

    fn GetAttributeValue(&self, id: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
        let doc = self.doc.doc()?;
        let r = self.span();
        t!("GetAttributeValue({}) {}", id.0, show(&doc, r));
        attribute(&doc, id, r)
    }

    fn GetBoundingRectangles(&self) -> Result<*mut SAFEARRAY> {
        Ok(rect_array(&self.doc.now()?.rects(self.span())))
    }

    fn GetEnclosingElement(&self) -> Result<IRawElementProviderSimple> {
        let path = self.doc.doc()?.enclosing(self.span());
        t!("GetEnclosingElement {:?} -> {:?}", self.span(), path);
        fragment(&self.doc, path)?.cast()
    }

    fn GetText(&self, max_length: i32) -> Result<BSTR> {
        let doc = self.doc.doc()?;
        let text = doc.text(self.span());
        let text = match usize::try_from(max_length) {
            Ok(n) if n < text.len() => &text[..n],
            _ => text,
        };
        t!("GetText({max_length}) {}", show(&doc, self.span()));
        Ok(BSTR::from_wide(text))
    }

    fn Move(&self, u: TextUnit, count: i32) -> Result<i32> {
        let doc = self.doc.doc()?;
        let before = self.span();
        let (s, moved) = doc.move_by(before, unit(u)?, count);
        self.set(s);
        t!(
            "Move({:?}, {count}) {} -> {} ({moved})",
            unit(u)?,
            show(&doc, before),
            show(&doc, s)
        );
        Ok(moved)
    }

    fn MoveEndpointByUnit(
        &self,
        endpoint: TextPatternRangeEndpoint,
        u: TextUnit,
        count: i32,
    ) -> Result<i32> {
        let doc = self.doc.doc()?;
        let before = self.span();
        let (s, moved) = doc.move_end(before, at_end(endpoint)?, unit(u)?, count);
        self.set(s);
        t!(
            "MoveEndpointByUnit(end={}, {:?}, {count}) {:?} -> {:?} ({moved})",
            at_end(endpoint)?,
            unit(u)?,
            before,
            s
        );
        Ok(moved)
    }

    fn MoveEndpointByRange(
        &self,
        endpoint: TextPatternRangeEndpoint,
        other: Ref<ITextRangeProvider>,
        other_endpoint: TextPatternRangeEndpoint,
    ) -> Result<()> {
        let other = self.other(&other)?;
        let o = other.span();
        let pos = if at_end(other_endpoint)? { o.1 } else { o.0 };
        let (s, e) = self.span();
        self.set(Indexed::set_end(s, e, at_end(endpoint)?, pos));
        t!(
            "MoveEndpointByRange(end={}, other end={}) {:?} -> {:?}",
            at_end(endpoint)?,
            at_end(other_endpoint)?,
            (s, e),
            self.span()
        );
        Ok(())
    }

    fn Select(&self) -> Result<()> {
        t!("Select {:?}", self.span());
        self.doc.select(self.span())
    }

    fn AddToSelection(&self) -> Result<()> {
        Err(invalid_operation())
    }

    fn RemoveFromSelection(&self) -> Result<()> {
        Err(invalid_operation())
    }

    fn ScrollIntoView(&self, _align_to_top: BOOL) -> Result<()> {
        t!("ScrollIntoView {:?}", self.span());
        Ok(())
    }

    fn GetChildren(&self) -> Result<*mut SAFEARRAY> {
        let kids = self.doc.doc()?.children_in(self.span());
        t!("GetChildren {:?} -> {:?}", self.span(), kids);
        let kids: Vec<IUnknown> = kids
            .into_iter()
            .map(|p| fragment(&self.doc, p)?.cast())
            .collect::<Result<_>>()?;
        Ok(safe_array_from_com_slice(&kids))
    }
}

impl ITextRangeProvider2_Impl for DocRange_Impl {
    fn ShowContextMenu(&self) -> Result<()> {
        Err(not_supported())
    }
}

/// The document node (empty path) or the element at `path`.
pub(crate) fn fragment(doc: &Doc, path: Vec<usize>) -> Result<IRawElementProviderFragment> {
    if path.is_empty() {
        doc.platform_node()
    } else {
        Ok(DocElement {
            doc: doc.clone(),
            path,
        }
        .into())
    }
}

/// Our element's path from its runtime id: `UiaAppendRuntimeId`, `OURS`, the
/// document node's id, then each index + 1.
fn path_of(own: &[i32; 5], ids: &[i32]) -> Option<Vec<usize>> {
    match ids {
        [_, ours, a, b, c, d, rest @ ..]
            if *ours == OURS && [*a, *b, *c, *d] == own[1..] && !rest.is_empty() =>
        {
            rest.iter()
                .map(|i| usize::try_from(*i).ok()?.checked_sub(1))
                .collect()
        }
        _ => None,
    }
}

/// The first or last element inside a document node, for the node's own
/// navigation.
pub(crate) fn child(doc: &Doc, last: bool) -> Result<Option<IRawElementProviderFragment>> {
    let n = doc.doc()?.children(&[]).len();
    if n == 0 {
        return Ok(None);
    }
    fragment(doc, vec![if last { n - 1 } else { 0 }]).map(Some)
}

/// An element inside the text: a link, a list or one of its items, a table
/// or one of its cells, a picture. Known by its path among the document's
/// elements.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IGridProvider,
    ITableProvider,
    IGridItemProvider,
    ITableItemProvider
)]
pub(crate) struct DocElement {
    doc: Doc,
    path: Vec<usize>,
}

impl DocElement {
    fn kind(&self) -> Result<ObjectKind> {
        self.doc
            .doc()?
            .object(&self.path)
            .map(|o| o.kind.clone())
            .ok_or_else(element_not_available)
    }

    fn parent(&self) -> Vec<usize> {
        self.path[..self.path.len().saturating_sub(1)].to_vec()
    }
}

/// Elements that have a place among their siblings: list items and sections.
fn numbered(kind: &ObjectKind) -> bool {
    matches!(kind, ObjectKind::ListItem | ObjectKind::Section)
}

fn trimmed(text: &[u16]) -> String {
    String::from_utf16_lossy(text)
        .trim_end_matches(['\r', '\n', '\u{7}'])
        .trim()
        .to_string()
}

impl IRawElementProviderSimple_Impl for DocElement_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, pattern_id: UIA_PATTERN_ID) -> Result<IUnknown> {
        t!(
            "Element {:?} GetPatternProvider({})",
            self.path,
            pattern_id.0
        );
        match (self.kind()?, pattern_id) {
            (ObjectKind::Table { .. }, UIA_GridPatternId) => {
                self.to_interface::<IGridProvider>().cast()
            }
            (ObjectKind::Table { .. }, UIA_TablePatternId) => {
                self.to_interface::<ITableProvider>().cast()
            }
            (ObjectKind::Cell { .. }, UIA_GridItemPatternId) => {
                self.to_interface::<IGridItemProvider>().cast()
            }
            (ObjectKind::Cell { .. }, UIA_TableItemPatternId) => {
                self.to_interface::<ITableItemProvider>().cast()
            }
            (ObjectKind::Cell { .. }, UIA_TextPatternId) => {
                let doc = self.doc.doc()?;
                let o = doc.object(&self.path).ok_or_else(element_not_available)?;
                let text: ITextProvider = PartText {
                    doc: self.doc.clone(),
                    span: (o.range.start, o.range.end),
                }
                .into();
                text.cast()
            }
            _ => Err(Error::empty()),
        }
    }

    fn GetPropertyValue(&self, property_id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let doc = self.doc.doc()?;
        let Some(o) = doc.object(&self.path) else {
            return Err(element_not_available());
        };
        let text = || trimmed(doc.text((o.range.start, o.range.end)));
        t!(
            "Element {:?} GetPropertyValue({})",
            self.path,
            property_id.0
        );
        Ok(match property_id {
            UIA_ControlTypePropertyId => match o.kind {
                ObjectKind::Link { .. } => UIA_HyperlinkControlTypeId,
                ObjectKind::List => UIA_ListControlTypeId,
                ObjectKind::ListItem => UIA_ListItemControlTypeId,
                ObjectKind::Table { .. } => UIA_TableControlTypeId,
                ObjectKind::Cell { .. } => UIA_DataItemControlTypeId,
                ObjectKind::Image { .. } => UIA_ImageControlTypeId,
                // A pane without a name: structure that screen readers don't announce, and
                // can number the section by its place. (A group would be read as a group box:
                // JAWS said "group" on every line in it.)
                ObjectKind::Section => UIA_PaneControlTypeId,
            }
            .0
            .into(),
            UIA_NamePropertyId => match &o.kind {
                ObjectKind::Link { .. } | ObjectKind::ListItem | ObjectKind::Cell { .. } => {
                    BSTR::from(text().as_str()).into()
                }
                ObjectKind::Image { alt } => BSTR::from(alt.as_str()).into(),
                ObjectKind::List | ObjectKind::Table { .. } | ObjectKind::Section => {
                    VARIANT::default()
                }
            },
            UIA_IsKeyboardFocusablePropertyId => matches!(o.kind, ObjectKind::Link { .. }).into(),
            // A list item's place in its list ("2 of 3"), and so the list's size; a section's
            // number and the number of sections.
            UIA_PositionInSetPropertyId if numbered(&o.kind) => {
                let at = self.path.last().map_or(0, |i| i + 1);
                i32::try_from(at).unwrap_or(i32::MAX).into()
            }
            UIA_SizeOfSetPropertyId if numbered(&o.kind) => {
                let n = doc.children(&self.parent()).len();
                i32::try_from(n).unwrap_or(i32::MAX).into()
            }
            // Lists in the text aren't nested, nor are sections: every one is at the first
            // level (JAWS reads a place in a list without a level as "level -1").
            UIA_LevelPropertyId if numbered(&o.kind) => 1.into(),
            UIA_IsEnabledPropertyId
            | UIA_IsContentElementPropertyId
            | UIA_IsControlElementPropertyId => true.into(),
            _ => VARIANT::default(),
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        Err(Error::empty())
    }
}

impl IRawElementProviderFragment_Impl for DocElement_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let doc = self.doc.doc()?;
        let parent = self.parent();
        let siblings = doc.children(&parent).len();
        let here = self.path.last().copied().unwrap_or(0);
        let kids = doc.children(&self.path).len();
        let with = |mut p: Vec<usize>, i: usize| {
            p.push(i);
            p
        };
        let to = match direction {
            NavigateDirection_Parent => Some(parent),
            NavigateDirection_NextSibling if here + 1 < siblings => Some(with(parent, here + 1)),
            NavigateDirection_PreviousSibling if here > 0 => Some(with(parent, here - 1)),
            NavigateDirection_FirstChild if kids > 0 => Some(with(self.path.clone(), 0)),
            NavigateDirection_LastChild if kids > 0 => Some(with(self.path.clone(), kids - 1)),
            _ => None,
        };
        t!(
            "Element {:?} Navigate({}) -> {:?}",
            self.path,
            direction.0,
            to
        );
        match to {
            Some(p) => fragment(&self.doc, p),
            None => Err(Error::empty()),
        }
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        let own = self.doc.node_ref_runtime_id();
        let mut ids = vec![UiaAppendRuntimeId as i32, OURS];
        ids.extend_from_slice(&own[1..]);
        ids.extend(
            self.path
                .iter()
                .map(|i| i32::try_from(i + 1).unwrap_or(i32::MAX)),
        );
        Ok(safe_array_from_i32_slice(&ids))
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let now = self.doc.now()?;
        let Some(o) = now.doc.object(&self.path) else {
            return Err(element_not_available());
        };
        Ok(uia_rect(&now.rects((o.range.start, o.range.end))))
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        self.doc.root()
    }
}

impl IGridProvider_Impl for DocElement_Impl {
    fn GetItem(&self, row: i32, column: i32) -> Result<IRawElementProviderSimple> {
        t!("Table {:?} GetItem({row}, {column})", self.path);
        let doc = self.doc.doc()?;
        let cell = doc.children(&self.path).iter().position(
            |c| matches!(c.kind, ObjectKind::Cell { row: r, col: k } if i32::try_from(r) == Ok(row) && i32::try_from(k) == Ok(column)),
        );
        match cell {
            Some(i) => {
                let mut p = self.path.clone();
                p.push(i);
                fragment(&self.doc, p)?.cast()
            }
            None => Err(invalid_arg()),
        }
    }

    fn RowCount(&self) -> Result<i32> {
        match self.kind()? {
            ObjectKind::Table { rows, .. } => Ok(i32::try_from(rows).unwrap_or(i32::MAX)),
            _ => Err(invalid_operation()),
        }
    }

    fn ColumnCount(&self) -> Result<i32> {
        match self.kind()? {
            ObjectKind::Table { cols, .. } => Ok(i32::try_from(cols).unwrap_or(i32::MAX)),
            _ => Err(invalid_operation()),
        }
    }
}

impl ITableProvider_Impl for DocElement_Impl {
    fn GetRowHeaders(&self) -> Result<*mut SAFEARRAY> {
        Ok(safe_array_from_com_slice(&[]))
    }

    fn GetColumnHeaders(&self) -> Result<*mut SAFEARRAY> {
        Ok(safe_array_from_com_slice(&[]))
    }

    fn RowOrColumnMajor(&self) -> Result<RowOrColumnMajor> {
        Ok(RowOrColumnMajor_RowMajor)
    }
}

impl IGridItemProvider_Impl for DocElement_Impl {
    fn Row(&self) -> Result<i32> {
        t!("Cell {:?} Row", self.path);
        match self.kind()? {
            ObjectKind::Cell { row, .. } => Ok(i32::try_from(row).unwrap_or(i32::MAX)),
            _ => Err(invalid_operation()),
        }
    }

    fn Column(&self) -> Result<i32> {
        t!("Cell {:?} Column", self.path);
        match self.kind()? {
            ObjectKind::Cell { col, .. } => Ok(i32::try_from(col).unwrap_or(i32::MAX)),
            _ => Err(invalid_operation()),
        }
    }

    fn RowSpan(&self) -> Result<i32> {
        Ok(1)
    }

    fn ColumnSpan(&self) -> Result<i32> {
        Ok(1)
    }

    fn ContainingGrid(&self) -> Result<IRawElementProviderSimple> {
        t!("Cell {:?} ContainingGrid", self.path);
        fragment(&self.doc, self.parent())?.cast()
    }
}

impl ITableItemProvider_Impl for DocElement_Impl {
    fn GetRowHeaderItems(&self) -> Result<*mut SAFEARRAY> {
        Ok(safe_array_from_com_slice(&[]))
    }

    fn GetColumnHeaderItems(&self) -> Result<*mut SAFEARRAY> {
        Ok(safe_array_from_com_slice(&[]))
    }
}

/// Whether a node is a document described as text.
pub(crate) fn is_document(node: &NodeRef) -> bool {
    node.data().text_document().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use accesskit::text_document::{Look, ParaKind};
    use accesskit::{TextDocument, TextDocumentRef};

    fn doc() -> Indexed {
        // "Red\r" on white, "Plain\r" with colours unknown.
        let kind = |line_spacing: &str| ParaKind {
            style_id: StyleId_Normal.0,
            style_name: "Normal".into(),
            align: Align::Left,
            line_spacing: line_spacing.into(),
        };
        let look = |color, background| Look {
            font: "Aptos".into(),
            size: 12.0,
            weight: 400,
            italic: false,
            underline: None,
            strike: None,
            superscript: false,
            subscript: false,
            color,
            background,
        };
        let rgb = |red, green, blue| Color {
            red,
            green,
            blue,
            alpha: 255,
        };
        Indexed::new(TextDocumentRef::from(TextDocument {
            text: "Red\rPlain\r".encode_utf16().collect(),
            paragraphs: vec![(0, kind("1.15")), (4, kind(""))],
            lines: vec![0, 4],
            pages: vec![0],
            words: vec![4],
            looks: vec![
                (0, look(Some(rgb(255, 0, 0)), Some(rgb(255, 255, 255)))),
                (4, look(None, None)),
            ],
            ..Default::default()
        }))
    }

    fn not_supported(v: &VARIANT) -> bool {
        let unknown = IUnknown::try_from(v).ok();
        unknown.is_some() && unknown == unsafe { UiaGetReservedNotSupportedValue() }.ok()
    }

    fn mixed(v: &VARIANT) -> bool {
        let unknown = IUnknown::try_from(v).ok();
        unknown.is_some() && unknown == unsafe { UiaGetReservedMixedAttributeValue() }.ok()
    }

    #[test]
    fn colours_are_colorrefs_where_known() {
        let d = doc();
        let fg = attribute(&d, UIA_ForegroundColorAttributeId, (0, 3)).unwrap();
        assert_eq!(i32::try_from(&fg).unwrap(), 0x0000FF);
        let bg = attribute(&d, UIA_BackgroundColorAttributeId, (0, 3)).unwrap();
        assert_eq!(i32::try_from(&bg).unwrap(), 0xFFFFFF);
        let plain = attribute(&d, UIA_ForegroundColorAttributeId, (4, 9)).unwrap();
        assert!(not_supported(&plain));
        let both = attribute(&d, UIA_ForegroundColorAttributeId, (0, 9)).unwrap();
        assert!(mixed(&both));
        // FindAttribute finds the red text.
        let red = VARIANT::from(0x0000FF);
        assert_eq!(
            find_attribute(&d, UIA_ForegroundColorAttributeId, &red, (0, 10), false),
            Some((0, 4))
        );
    }

    #[test]
    fn line_spacing_is_the_applications_text() {
        let d = doc();
        let v = attribute(&d, UIA_LineSpacingAttributeId, (1, 1)).unwrap();
        assert_eq!(text_of(&v).as_deref(), Some("1.15"));
        let unknown = attribute(&d, UIA_LineSpacingAttributeId, (5, 5)).unwrap();
        assert!(not_supported(&unknown));
        assert_eq!(attribute_unit(UIA_LineSpacingAttributeId), Unit::Paragraph);
    }
}
