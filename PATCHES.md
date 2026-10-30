# Patches in this fork

This is a fork of [AccessKit](https://github.com/AccessKit/accesskit) for
applications that draw a long, editable document themselves, such as a word
processor. It adds a way to describe such a document as one text, as word
processors expose their own documents to screen readers, instead of one node
per paragraph and run.

Base: AccessKit `ce8164ba` (release of `accesskit` 0.25.1,
`accesskit_consumer` 0.39.1, `accesskit_windows` 0.35.1, `accesskit_winit`
0.34.1). Versions are unchanged, so a `[patch.crates-io]` entry pointing at
this branch replaces those crates.

## 1. `text_document` and `page_bounds` node properties (`accesskit`)

- `accesskit::text_document` holds the document as plain data: the whole text
  (UTF-16), where paragraphs, laid-out lines, pages and words start, each
  paragraph's UI Automation style id, style name, alignment and line
  spacing, the formatting of each stretch of text (with its colours on
  screen), spelling and grammar errors, the
  elements inside the text (links, lists and their items, tables and their
  cells, images with their alternative text) and where each line sits on its
  page.
- `Node::text_document` carries it as a shared `TextDocumentRef`, so an
  application builds it once per change and comparing two updates is cheap.
- `Node::page_bounds` places each page in the node's coordinate space.
- The node's `text_selection` uses offsets into the document's text, with the
  node itself as the position's node.
- Both properties serialize with `serde`. `page_bounds` is in the JSON
  schema; `text_document` isn't yet.

Adapters that don't support the property ignore it.

## 2. Windows: a node with a text document has its own text pattern (`accesskit_windows`)

`adapters/windows/src/document` serves the document through UI Automation
the way Microsoft Word does:

- `TextPattern` and `TextPattern2` over the whole text, with character,
  format, word, line, paragraph, page and document units, `FindText`,
  `FindAttribute`, visible ranges, `RangeFromPoint`, bounding rectangles from
  the line boxes and the caret range.
- Text attributes: font name, size, weight, italic, underline, strikethrough,
  superscript, subscript, foreground and background colour, `StyleId`, style
  name, alignment, line spacing, annotation types (spelling and grammar
  errors). `IsReadOnly` is false: the document is editable, so screen
  readers echo typing. An attribute the application leaves unknown (a colour,
  the line spacing) is not supported.
- The elements inside the text are UI Automation elements under the document
  node: hyperlinks, lists and list items (with their place in the list,
  `PositionInSet` and `SizeOfSet`, at `Level` 1: the lists aren't nested),
  tables with `Grid` and `Table`
  patterns, cells with `GridItem`, `TableItem` and a text pattern over their
  own text, and images named by their alternative text.
- `Select` on a range is sent to the application as `SetTextSelection` on the
  node.
- A change to the document's text raises `TextChanged`.
- Ranges from these documents and AccessKit's own text ranges never mix:
  each side rejects the other's.

The `document-trace` feature (off by default) logs the calls screen readers
make on these documents to the file named by `ACCESSKIT_DOC_LOG`, to see how
they read them.

## Tests

- `document::units` tests: moving and expanding by each unit, surrogate
  pairs, attributes over a range, elements, `FindText`.
- `document::tests`: colours as COLORREFs, unknown, mixed and found with
  `FindAttribute`; the line spacing as the application's text.
- `tests::document`: a window with a text document read through the UI
  Automation client: the whole text, `IsReadOnly` false, the heading's
  `StyleId`, the link as a hyperlink element, each list item's place in its
  list.
