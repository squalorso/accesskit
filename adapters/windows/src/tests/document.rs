// Copyright 2026 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

use accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, Node, NodeId, Role, TextDocument, TreeId,
    TreeInfo, TreeUpdate,
    text_document::{Align, Look, Object, ObjectKind, ParaKind},
};
use windows::{
    Win32::{System::Variant::VARIANT, UI::Accessibility::*},
    core::*,
};

use super::Scope;

const WINDOW_TITLE: &str = "Document test";

const WINDOW_ID: NodeId = NodeId(0);
const DOCUMENT_ID: NodeId = NodeId(1);

const TEXT: &str = "Title\rOne two\r";

fn paragraph(style_id: i32, style_name: &str) -> ParaKind {
    ParaKind {
        style_id,
        style_name: style_name.into(),
        align: Align::Left,
    }
}

fn get_initial_state() -> TreeUpdate {
    let mut root = Node::new(Role::Window);
    root.set_children(vec![DOCUMENT_ID]);
    let mut document = Node::new(Role::Document);
    document.set_label("Document");
    document.set_text_document(TextDocument {
        text: TEXT.encode_utf16().collect(),
        paragraphs: vec![
            (0, paragraph(StyleId_Heading1.0, "Heading 1")),
            (6, paragraph(StyleId_Normal.0, "Normal")),
        ],
        lines: vec![0, 6],
        pages: vec![0],
        words: vec![0, 6, 10],
        looks: vec![(
            0,
            Look {
                font: "Arial".into(),
                size: 12.0,
                weight: 400,
                italic: false,
                underline: None,
                strike: None,
                superscript: false,
                subscript: false,
            },
        )],
        annotations: vec![],
        objects: vec![Object {
            kind: ObjectKind::Link {
                url: "https://example.com/".into(),
            },
            range: 10..13,
            children: vec![],
        }],
        boxes: vec![],
    });
    TreeUpdate {
        nodes: vec![(WINDOW_ID, root), (DOCUMENT_ID, document)],
        tree: Some(TreeInfo::new(WINDOW_ID)),
        tree_id: TreeId::ROOT,
        focus: DOCUMENT_ID,
    }
}

pub struct NullActionHandler;

impl ActionHandler for NullActionHandler {
    fn do_action(&mut self, _request: ActionRequest) {}
}

struct DocumentActivationHandler;

impl ActivationHandler for DocumentActivationHandler {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        Some(get_initial_state())
    }
}

fn scope<F>(f: F) -> Result<()>
where
    F: FnOnce(&Scope) -> Result<()>,
{
    super::scope(
        WINDOW_TITLE,
        DocumentActivationHandler {},
        NullActionHandler {},
        f,
    )
}

fn document(s: &Scope) -> Result<IUIAutomationElement> {
    let root = unsafe { s.uia.ElementFromHandle(s.window.0) }?;
    let condition = unsafe {
        s.uia.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &VARIANT::from(UIA_DocumentControlTypeId.0),
        )
    }?;
    unsafe { root.FindFirst(TreeScope_Descendants, &condition) }
}

fn document_range(s: &Scope) -> Result<IUIAutomationTextRange> {
    let document = document(s)?;
    let pattern: IUIAutomationTextPattern =
        unsafe { document.GetCurrentPatternAs(UIA_TextPatternId) }?;
    unsafe { pattern.DocumentRange() }
}

#[test]
fn whole_text() -> Result<()> {
    scope(|s| {
        let range = document_range(s)?;
        let text = unsafe { range.GetText(-1) }?;
        assert_eq!(text.to_string(), TEXT);
        Ok(())
    })
}

#[test]
fn not_read_only() -> Result<()> {
    // Screen readers echo typing only in text that isn't read-only.
    scope(|s| {
        let range = document_range(s)?;
        let read_only = unsafe { range.GetAttributeValue(UIA_IsReadOnlyAttributeId) }?;
        assert!(!bool::try_from(&read_only)?);
        Ok(())
    })
}

#[test]
fn heading_style() -> Result<()> {
    scope(|s| {
        let range = document_range(s)?;
        unsafe {
            range.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &range,
                TextPatternRangeEndpoint_Start,
            )
        }?;
        unsafe { range.ExpandToEnclosingUnit(TextUnit_Paragraph) }?;
        assert_eq!(unsafe { range.GetText(-1) }?.to_string(), "Title\r");
        let style = unsafe { range.GetAttributeValue(UIA_StyleIdAttributeId) }?;
        assert_eq!(i32::try_from(&style)?, StyleId_Heading1.0);
        Ok(())
    })
}

#[test]
fn link_element() -> Result<()> {
    scope(|s| {
        let document = document(s)?;
        let walker = unsafe { s.uia.RawViewWalker() }?;
        let link = unsafe { walker.GetFirstChildElement(&document) }?;
        assert_eq!(
            unsafe { link.CurrentControlType() }?,
            UIA_HyperlinkControlTypeId
        );
        assert_eq!(unsafe { link.CurrentName() }?.to_string(), "two");
        Ok(())
    })
}
