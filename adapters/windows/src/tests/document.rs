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

const TEXT: &str = "Title\rOne two\r\u{2022} A\r\u{2022} B\r";

fn paragraph(style_id: i32, style_name: &str) -> ParaKind {
    ParaKind {
        style_id,
        style_name: style_name.into(),
        align: Align::Left,
        line_spacing: "1.15".into(),
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
            (14, paragraph(StyleId_BulletedList.0, "List Paragraph")),
            (18, paragraph(StyleId_BulletedList.0, "List Paragraph")),
        ],
        lines: vec![0, 6, 14, 18],
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
                color: None,
                background: None,
            },
        )],
        annotations: vec![],
        // Two sections: the heading and the link, then the list.
        objects: vec![
            Object {
                kind: ObjectKind::Section,
                range: 0..14,
                children: vec![Object {
                    kind: ObjectKind::Link {
                        url: "https://example.com/".into(),
                    },
                    range: 10..13,
                    children: vec![],
                }],
            },
            Object {
                kind: ObjectKind::Section,
                range: 14..22,
                children: vec![Object {
                    kind: ObjectKind::List,
                    range: 14..22,
                    children: vec![
                        Object {
                            kind: ObjectKind::ListItem,
                            range: 14..18,
                            children: vec![],
                        },
                        Object {
                            kind: ObjectKind::ListItem,
                            range: 18..22,
                            children: vec![],
                        },
                    ],
                }],
            },
        ],
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

/// An element's place among its siblings and their number.
fn place(element: &IUIAutomationElement) -> Result<(i32, i32)> {
    let at = unsafe { element.GetCurrentPropertyValue(UIA_PositionInSetPropertyId) }?;
    let of = unsafe { element.GetCurrentPropertyValue(UIA_SizeOfSetPropertyId) }?;
    Ok((i32::try_from(&at)?, i32::try_from(&of)?))
}

#[test]
fn link_element() -> Result<()> {
    scope(|s| {
        let document = document(s)?;
        let walker = unsafe { s.uia.RawViewWalker() }?;
        let section = unsafe { walker.GetFirstChildElement(&document) }?;
        let link = unsafe { walker.GetFirstChildElement(&section) }?;
        assert_eq!(
            unsafe { link.CurrentControlType() }?,
            UIA_HyperlinkControlTypeId
        );
        assert_eq!(unsafe { link.CurrentName() }?.to_string(), "two");
        Ok(())
    })
}

#[test]
fn sections_are_numbered_panes() -> Result<()> {
    scope(|s| {
        let document = document(s)?;
        let walker = unsafe { s.uia.RawViewWalker() }?;
        let first = unsafe { walker.GetFirstChildElement(&document) }?;
        let second = unsafe { walker.GetNextSiblingElement(&first) }?;
        for (section, at) in [(&first, 1), (&second, 2)] {
            assert_eq!(
                unsafe { section.CurrentControlType() }?,
                UIA_PaneControlTypeId
            );
            assert_eq!(unsafe { section.CurrentName() }?.to_string(), "");
            assert_eq!(place(section)?, (at, 2));
        }
        // A position inside the list is inside the second section.
        let pattern: IUIAutomationTextPattern =
            unsafe { document.GetCurrentPatternAs(UIA_TextPatternId) }?;
        let range = unsafe { pattern.RangeFromChild(&second) }?;
        let text = unsafe { range.GetText(-1) }?.to_string();
        assert_eq!(text, "\u{2022} A\r\u{2022} B\r");
        Ok(())
    })
}

#[test]
fn list_items_know_their_place() -> Result<()> {
    scope(|s| {
        let document = document(s)?;
        let walker = unsafe { s.uia.RawViewWalker() }?;
        let first = unsafe { walker.GetFirstChildElement(&document) }?;
        let second = unsafe { walker.GetNextSiblingElement(&first) }?;
        let list = unsafe { walker.GetFirstChildElement(&second) }?;
        assert_eq!(unsafe { list.CurrentControlType() }?, UIA_ListControlTypeId);
        let first = unsafe { walker.GetFirstChildElement(&list) }?;
        assert_eq!(place(&first)?, (1, 2));
        let last = unsafe { walker.GetLastChildElement(&list) }?;
        assert_eq!(place(&last)?, (2, 2));
        Ok(())
    })
}
