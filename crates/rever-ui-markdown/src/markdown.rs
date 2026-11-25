//! Port of `src/renderer/src/components/chat/Markdown.tsx` — chat messages as Markdown.
//!
//! react-markdown plus remark-gfm become pulldown-cmark plus GPUI elements. [`parse`]
//! turns the text into plain [`Block`]s (no GPUI, unit-tested); [`markdown`] draws them
//! with the `.chat-log .md` rules of `src/renderer/src/styles.css`.

use std::fmt::Write as _;
use std::iter::Peekable;
use std::ops::Range;
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Duration;

use gpui::{
    AnyElement, App, ClipboardItem, CursorStyle, Div, ElementId, Font, FontStyle, FontWeight, Hsla,
    InteractiveText, MouseButton, Rgba, SharedString, StrikethroughStyle, StyledText, TextRun,
    Window, actions, canvas, div, font, prelude::*, px, relative, rgb, rgba,
};
pub use pulldown_cmark::Alignment;
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd, TextMergeStream};
use regex::Regex;
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::Theme;

use crate::selection;

actions!(markdown, [CopySelection]);

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// A stretch of inline text with one style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// The link target. Empty when the URL was rejected by [`safe_url`]: the text
    /// still looks like a link, as `<a href="">` does, but a click reports nothing.
    pub link: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Paragraph(Vec<Span>),
    /// Inline content with no `<p>` around it: a tight list item's text, raw HTML.
    Text(Vec<Span>),
    Heading {
        level: u8,
        content: Vec<Span>,
    },
    /// The text of a `<pre>`, trailing newline included.
    Code(String),
    List {
        /// The first number of an ordered list; `None` for a bullet list.
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    Quote(Vec<Block>),
    Table {
        aligns: Vec<Alignment>,
        head: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
    Rule,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    /// `Some(checked)` for a `- [ ]` / `- [x]` item.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

/// Quotes and lists nested deeper than this are flattened into their parent. The
/// parser, the element tree and its layout all recurse once per level, and a line of
/// ten thousand `>` must not overflow the stack.
pub const MAX_DEPTH: usize = 16;

/// Parse a message. Never fails: incomplete Markdown (a message still streaming)
/// parses as far as it goes — an unclosed fence is a code block up to the end.
pub fn parse(text: &str) -> Vec<Block> {
    // Footnotes are part of remark-gfm. Without the option `[^1]` plus its definition
    // would parse as a reference link named "^1" pointing at the note's text.
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut reader = Reader {
        events: TextMergeStream::new(Parser::new_ext(text, options)).peekable(),
        footnotes: Footnotes::default(),
    };
    let mut blocks = reader.blocks(0, &mut None);
    blocks.extend(reader.footnotes.footer());
    blocks
}

/// The href of a link as react-markdown renders it: percent-encoded, then emptied
/// when its protocol is not allowed.
fn href(url: &str) -> String {
    safe_url(&normalize_uri(url)).to_owned()
}

/// micromark's `normalizeUri`: percent-encodes everything outside the characters a
/// URL may hold, and leaves a `%` alone when two ASCII letters or digits follow it.
pub fn normalize_uri(url: &str) -> String {
    let bytes = url.as_bytes();
    let mut out = String::with_capacity(url.len());
    for (at, c) in url.char_indices() {
        let escape = c == '%'
            && (1..=2).all(|ahead| bytes.get(at + ahead).is_some_and(u8::is_ascii_alphanumeric));
        if escape
            || matches!(c, '!' | '#' | '$' | '&'..=';' | '=' | '?'..='Z' | '_' | 'a'..='z' | '~')
        {
            out.push(c);
        } else {
            for byte in c.encode_utf8(&mut [0; 4]).bytes() {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// react-markdown's `defaultUrlTransform`: relative URLs and the protocols http,
/// https, irc, ircs, mailto and xmpp pass; anything else (`javascript:` …) becomes "".
pub fn safe_url(url: &str) -> &str {
    let Some(colon) = url.find(':') else {
        return url;
    };
    // A colon after `/`, `?` or `#` is not a protocol separator.
    if ['/', '?', '#']
        .iter()
        .any(|&c| url.find(c).is_some_and(|at| colon > at))
    {
        return url;
    }
    const SAFE: [&str; 6] = ["http", "https", "irc", "ircs", "mailto", "xmpp"];
    let protocol = url.get(..colon).unwrap_or_default();
    if SAFE.iter().any(|safe| protocol.eq_ignore_ascii_case(safe)) {
        url
    } else {
        ""
    }
}

// ---------------------------------------------------------------------------
// Autolink literals
// ---------------------------------------------------------------------------
//
// remark-gfm links bare URLs and e-mail addresses; pulldown-cmark does not. remark
// does it twice: micromark-extension-gfm-autolink-literal while tokenizing, then
// mdast-util-gfm-autolink-literal over the text that is left. The first pass links
// `http://localhost:3000` (no dot needed), the second `"www.example.com"` (after
// any punctuation), so both are ported and tried at every position.
//
// shortcut: this reads parsed text, not the source. A URL holding emphasis that
// pulldown-cmark pairs (`https://a.b/*x*`) is cut at the marker, and `&amp;` is
// already `&` when the trailing-reference rule looks at it. Upgrade by scanning the
// source through the parser's offsets if such URLs show up.

/// JavaScript's `\p{P}|\p{S}`, which the standard library cannot answer.
static PUNCTUATION: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"^[\p{P}\p{S}]$").ok());

/// micromark's `unicodePunctuation`. It tests one UTF-16 unit, so a character
/// outside the BMP (an emoji) is never punctuation.
fn is_punctuation(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_punctuation();
    }
    c as u32 <= 0xFFFF
        && PUNCTUATION
            .as_ref()
            .is_some_and(|punctuation| punctuation.is_match(c.encode_utf8(&mut [0; 4])))
}

/// JavaScript's `\s`: Unicode White_Space without U+0085, plus U+FEFF.
fn is_js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

fn is_atext(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.' | '_')
}

fn starts_with_ignore_case(chars: &[char], at: usize, prefix: &str) -> bool {
    prefix.chars().enumerate().all(|(offset, expected)| {
        chars
            .get(at + offset)
            .is_some_and(|c| c.eq_ignore_ascii_case(&expected))
    })
}

/// `tokenizeTrail`: whether everything from `at` up to the next space, `<` or the
/// end is punctuation that belongs to the sentence, not to the URL.
fn is_trail(chars: &[char], mut at: usize) -> bool {
    loop {
        match chars.get(at).copied() {
            None | Some('<') => return true,
            Some(c) if is_js_space(c) => return true,
            Some('!' | '"' | '\'' | ')' | '*' | ',' | '.' | ':' | ';' | '?' | '_' | '~') => at += 1,
            // `&name;` at the end is a character reference, not a query parameter.
            Some('&') => {
                let name = chars
                    .get(at + 1..)
                    .unwrap_or_default()
                    .iter()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .count();
                if name == 0 || chars.get(at + 1 + name) != Some(&';') {
                    return false;
                }
                at += name + 2;
            }
            Some(']') => {
                at += 1;
                match chars.get(at).copied() {
                    None | Some('(' | '[') => return true,
                    Some(c) if is_js_space(c) => return true,
                    Some(_) => {}
                }
            }
            Some(_) => return false,
        }
    }
}

/// `tokenizeDomain`: the end of the domain starting at `at`. An underscore in one
/// of the last two segments, or an empty domain, is no domain.
fn domain_end(chars: &[char], mut at: usize) -> Option<usize> {
    let (mut underscore_last, mut underscore_before_last, mut seen) = (false, false, false);
    loop {
        match chars.get(at).copied() {
            Some(c @ ('.' | '_')) => {
                if is_trail(chars, at) {
                    break;
                }
                if c == '_' {
                    underscore_last = true;
                } else {
                    underscore_before_last = underscore_last;
                    underscore_last = false;
                }
            }
            None => break,
            Some(c) if is_js_space(c) || (c != '-' && is_punctuation(c)) => break,
            Some(_) => seen = true,
        }
        at += 1;
    }
    (seen && !underscore_last && !underscore_before_last).then_some(at)
}

/// `tokenizePath`: the end of the path starting at `at`. A `)` stays in the URL
/// while it closes a `(` of the path.
fn path_end(chars: &[char], mut at: usize) -> usize {
    let (mut open, mut close) = (0usize, 0usize);
    loop {
        match chars.get(at).copied() {
            Some('(') => open += 1,
            Some(')') if close < open => close += 1,
            Some(
                c @ ('!' | '"' | '&' | '\'' | ')' | '*' | ',' | '.' | ':' | ';' | '<' | '?' | ']'
                | '_' | '~'),
            ) => {
                if is_trail(chars, at) {
                    return at;
                }
                if c == ')' {
                    close += 1;
                }
            }
            None => return at,
            Some(c) if is_js_space(c) => return at,
            Some(_) => {}
        }
        at += 1;
    }
}

/// `tokenizeEmailAutolink`: the end of the address starting at `at`. The domain
/// needs a dot and must end in a letter.
fn email_end(chars: &[char], at: usize) -> Option<usize> {
    let rest = chars.get(at..)?;
    let mut end = at + rest.iter().take_while(|c| is_atext(**c)).count();
    if chars.get(end) != Some(&'@') {
        return None;
    }
    end += 1;
    let (mut dot, mut data) = (false, false);
    loop {
        match chars.get(end).copied() {
            // A dot with no letter or digit after it ends the sentence.
            Some('.') if chars.get(end + 1).is_some_and(char::is_ascii_alphanumeric) => dot = true,
            Some(c) if c == '-' || c == '_' || c.is_ascii_alphanumeric() => data = true,
            _ => break,
        }
        end += 1;
    }
    let last = chars.get(end.checked_sub(1)?)?;
    (data && dot && last.is_ascii_alphabetic()).then_some(end)
}

/// The mdast pass: `/(https?:\/\/|www(?=\.))([-.\w]+)([^ \t\r\n]*)/gi` with
/// `isCorrectDomain` and `splitUrl`. Returns the end of the link.
fn loose_url_end(chars: &[char], at: usize) -> Option<usize> {
    let (domain_from, body_from) = if starts_with_ignore_case(chars, at, "https://") {
        (at + 8, at + 8)
    } else if starts_with_ignore_case(chars, at, "http://") {
        (at + 7, at + 7)
    } else if starts_with_ignore_case(chars, at, "www.") {
        // `www` counts as part of the domain.
        (at + 3, at)
    } else {
        return None;
    };
    let rest = chars.get(domain_from..)?;
    let domain_len = rest
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
        .count();
    if domain_len == 0 {
        return None;
    }
    let domain: String = chars
        .get(body_from..domain_from + domain_len)?
        .iter()
        .collect();
    let parts: Vec<&str> = domain.split('.').collect();
    let wrong = |part: &&str| {
        !part.is_empty()
            && (part.contains('_') || !part.contains(|c: char| c.is_ascii_alphanumeric()))
    };
    if parts.len() < 2 || parts.iter().rev().take(2).any(wrong) {
        return None;
    }
    let stop = domain_from
        + rest
            .iter()
            .take_while(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
            .count();
    let body = chars.get(body_from..stop)?;
    let trail = body
        .iter()
        .rev()
        .take_while(|c| "!\"&'),.:;<>?]}".contains(**c))
        .count();
    let mut end = stop - trail;
    // A `)` of the trail goes back into the URL for each `(` still open in it.
    let url = chars.get(body_from..end)?;
    let open = url.iter().filter(|c| **c == '(').count();
    let mut close = url.iter().filter(|c| **c == ')').count();
    while open > close {
        let Some(paren) = chars.get(end..stop)?.iter().position(|c| *c == ')') else {
            break;
        };
        end += paren + 1;
        close += 1;
    }
    (end > body_from).then_some(end)
}

/// The autolink literal starting exactly at `at`, as (end, target).
fn autolink_at(chars: &[char], at: usize, previous: Option<char>) -> Option<(usize, String)> {
    let first = *chars.get(at)?;
    let text = |end: usize| {
        chars
            .get(at..end)
            .map(|link| link.iter().collect::<String>())
    };
    // micromark tries the e-mail address first, then the URL.
    if is_atext(first)
        && !previous.is_some_and(|p| p == '/' || is_atext(p))
        && let Some(end) = email_end(chars, at)
    {
        return Some((end, format!("mailto:{}", text(end)?)));
    }
    if !previous.is_some_and(|p| p.is_ascii_alphabetic())
        && let Some(prefix) = ["https://", "http://"]
            .into_iter()
            .find(|prefix| starts_with_ignore_case(chars, at, prefix))
        // Something that can start a domain has to follow the slashes.
        && chars.get(at + prefix.len()).is_some_and(|next| {
            !next.is_ascii_control() && !is_js_space(*next) && !is_punctuation(*next)
        })
        && let Some(domain) = domain_end(chars, at + prefix.len())
    {
        let end = path_end(chars, domain);
        return Some((end, text(end)?));
    }
    if previous.is_none_or(|p| "(*_[]~ \t\r\n".contains(p))
        && starts_with_ignore_case(chars, at, "www.")
        && chars.get(at + 4).is_some()
        && let Some(domain) = domain_end(chars, at)
    {
        let end = path_end(chars, domain);
        return Some((end, format!("http://{}", text(end)?)));
    }
    // shortcut: the mdast pass for e-mail addresses is left out. It differs from the
    // tokenizer above only for a domain segment that starts with `-` or `_`.
    if previous.is_none_or(|p| is_js_space(p) || is_punctuation(p))
        && let Some(end) = loose_url_end(chars, at)
    {
        let link = text(end)?;
        return Some(if starts_with_ignore_case(chars, at, "www.") {
            (end, format!("http://{link}"))
        } else {
            (end, link)
        });
    }
    None
}

/// The first bare URL or e-mail address in `text` as (start, end, target), in bytes.
/// `previous` is the character before `text`, if it continues a paragraph.
fn find_autolink(text: &str, mut previous: Option<char>) -> Option<(usize, usize, String)> {
    // Every literal has one of these; text without them is not scanned.
    if !text.contains(['@', ':', '.']) {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut start = 0usize;
    for (at, c) in chars.iter().enumerate() {
        if let Some((end, target)) = autolink_at(&chars, at, previous) {
            let len: usize = chars.get(at..end)?.iter().map(|c| c.len_utf8()).sum();
            return Some((start, start + len, target));
        }
        start += c.len_utf8();
        previous = Some(*c);
    }
    None
}

/// Inline events belong to the paragraph being read; anything else ends it. `open`
/// counts the inline tags still open, whose `End` is inline too.
fn is_inline(event: &Event, open: usize) -> bool {
    match event {
        Event::Start(tag) => matches!(
            tag,
            Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }
        ),
        Event::End(_) => open > 0,
        Event::Rule => false,
        _ => true,
    }
}

struct InlineState {
    spans: Vec<Span>,
    bold: usize,
    italic: usize,
    strike: usize,
    links: Vec<String>,
    /// `white-space: normal`: the next space would follow a space or start a line,
    /// and is dropped as a browser drops it.
    after_space: bool,
}

impl InlineState {
    fn new() -> Self {
        Self {
            spans: Vec::new(),
            bold: 0,
            italic: 0,
            strike: 0,
            links: Vec::new(),
            after_space: true,
        }
    }

    /// Text as HTML shows it: runs of spaces, tabs and newlines are one space, also
    /// across spans. No-break spaces stay.
    fn push(&mut self, text: &str, code: bool) {
        let mut shown = String::with_capacity(text.len());
        for c in text.chars() {
            if matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
                if !self.after_space {
                    shown.push(' ');
                }
                self.after_space = true;
            } else {
                shown.push(c);
                self.after_space = false;
            }
        }
        self.push_span(&shown, code);
    }

    /// `<br>`: the one newline that is kept.
    fn line_break(&mut self) {
        self.push_span("\n", false);
        self.after_space = true;
    }

    fn push_span(&mut self, text: &str, code: bool) {
        if text.is_empty() {
            return;
        }
        let span = Span {
            text: String::new(),
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code,
            link: self.links.last().cloned(),
        };
        match self.spans.last_mut() {
            Some(last)
                if (last.bold, last.italic, last.strike, last.code, &last.link)
                    == (span.bold, span.italic, span.strike, span.code, &span.link) =>
            {
                last.text.push_str(text)
            }
            _ => self.spans.push(Span {
                text: text.to_owned(),
                ..span
            }),
        }
    }

    fn push_text(&mut self, text: &str) {
        let mut rest = text;
        let mut previous = None;
        // Text that is already a link is not linked again.
        while self.links.is_empty() {
            let Some((start, end, target)) = find_autolink(rest, previous) else {
                break;
            };
            self.push(rest.get(..start).unwrap_or_default(), false);
            let literal = rest.get(start..end).unwrap_or_default();
            self.links.push(href(&target));
            self.push(literal, false);
            self.links.pop();
            previous = literal.chars().next_back();
            rest = rest.get(end..).unwrap_or_default();
        }
        self.push(rest, false);
    }
}

/// Footnotes as mdast-util-to-hast lays them out: a reference is the number of the
/// note, and the notes that are referenced come after the message, in that order.
#[derive(Default)]
struct Footnotes {
    /// (id, times referenced), in order of first reference.
    // shortcut: a reference inside a note is numbered where the note stands in the
    // source; remark numbers it after every reference of the message itself.
    order: Vec<(String, usize)>,
    definitions: Vec<(String, Vec<Block>)>,
}

impl Footnotes {
    /// Labels match without regard to case; the id is what ends up in the href.
    fn id(label: &str) -> String {
        normalize_uri(&label.to_lowercase())
    }

    /// Counts a reference; returns the note's number and its href.
    fn reference(&mut self, label: &str) -> (usize, String) {
        let id = Self::id(label);
        let number = match self.order.iter().position(|(known, _)| *known == id) {
            Some(index) => index + 1,
            None => {
                self.order.push((id.clone(), 0));
                self.order.len()
            }
        };
        if let Some((_, count)) = self.order.get_mut(number - 1) {
            *count += 1;
        }
        (number, format!("#user-content-fn-{id}"))
    }

    /// `<section class="footnotes">`: the "Footnotes" heading (its `sr-only` class has
    /// no rule in styles.css, so it shows) and the numbered notes, each ending in a
    /// link back to every reference.
    fn footer(self) -> Vec<Block> {
        let Self { order, definitions } = self;
        let mut items = Vec::new();
        for (id, count) in order {
            // The first definition of a label wins.
            let Some((_, blocks)) = definitions.iter().find(|(known, _)| *known == id) else {
                continue;
            };
            let mut blocks = blocks.clone();
            let mut back = Vec::new();
            for nth in 1..=count {
                if nth > 1 {
                    back.push(Span {
                        text: " ".into(),
                        ..Span::default()
                    });
                }
                back.push(Span {
                    text: if nth > 1 {
                        format!("↩{nth}")
                    } else {
                        "↩".into()
                    },
                    link: Some(if nth > 1 {
                        format!("#user-content-fnref-{id}-{nth}")
                    } else {
                        format!("#user-content-fnref-{id}")
                    }),
                    ..Span::default()
                });
            }
            match blocks.last_mut() {
                Some(Block::Paragraph(spans)) => {
                    // The space goes into the note's last text when that is plain.
                    let plain = |span: &Span| {
                        !(span.bold || span.italic || span.strike || span.code)
                            && span.link.is_none()
                    };
                    match spans.last_mut() {
                        Some(last) if plain(last) => last.text.push(' '),
                        _ => spans.push(Span {
                            text: " ".into(),
                            ..Span::default()
                        }),
                    }
                    spans.append(&mut back);
                }
                _ => blocks.push(Block::Text(back)),
            }
            items.push(ListItem { task: None, blocks });
        }
        if items.is_empty() {
            return Vec::new();
        }
        vec![
            Block::Heading {
                level: 2,
                content: vec![Span {
                    text: "Footnotes".into(),
                    ..Span::default()
                }],
            },
            Block::List {
                start: Some(1),
                items,
            },
        ]
    }
}

struct Reader<'a> {
    events: Peekable<TextMergeStream<'a, Parser<'a>>>,
    footnotes: Footnotes,
}

impl Reader<'_> {
    fn end_tag(&mut self) {
        self.events.next_if(|event| matches!(event, Event::End(_)));
    }

    /// Reads up to the `End` of the enclosing container, which it consumes.
    fn blocks(&mut self, depth: usize, task: &mut Option<bool>) -> Vec<Block> {
        let mut out = Vec::new();
        // Containers entered without a block of their own; their `End` is not ours.
        let mut transparent = 0usize;
        loop {
            match self.events.peek() {
                None => break,
                Some(event) if is_inline(event, 0) => {
                    let spans = self.inline(task);
                    if !spans.is_empty() {
                        out.push(Block::Text(spans));
                    }
                    continue;
                }
                Some(_) => {}
            }
            let Some(event) = self.events.next() else {
                break;
            };
            match event {
                Event::Start(Tag::Paragraph) => {
                    let spans = self.inline(task);
                    self.end_tag();
                    out.push(Block::Paragraph(spans));
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    let content = self.inline(task);
                    self.end_tag();
                    out.push(Block::Heading {
                        level: level as u8,
                        content,
                    });
                }
                Event::Start(Tag::CodeBlock(_)) => {
                    let mut code = String::new();
                    while let Some(Event::Text(text)) =
                        self.events.next_if(|event| matches!(event, Event::Text(_)))
                    {
                        code.push_str(&text);
                    }
                    self.end_tag();
                    out.push(Block::Code(code));
                }
                // react-markdown shows raw HTML as its source text, outside any <p>.
                Event::Start(Tag::HtmlBlock) => {
                    let spans = self.inline(task);
                    self.end_tag();
                    if !spans.is_empty() {
                        out.push(Block::Text(spans));
                    }
                }
                Event::Start(Tag::BlockQuote(_)) if depth < MAX_DEPTH => {
                    out.push(Block::Quote(self.blocks(depth + 1, &mut None)));
                }
                // Shown by `Footnotes::footer`, not where it is written.
                Event::Start(Tag::FootnoteDefinition(label)) if depth < MAX_DEPTH => {
                    let blocks = self.blocks(depth + 1, &mut None);
                    self.footnotes
                        .definitions
                        .push((Footnotes::id(&label), blocks));
                }
                Event::Start(Tag::List(start)) if depth < MAX_DEPTH => {
                    let mut items = Vec::new();
                    while let Some(Event::Start(Tag::Item)) = self.events.next() {
                        let mut task = None;
                        let blocks = self.blocks(depth + 1, &mut task);
                        items.push(ListItem { task, blocks });
                    }
                    out.push(Block::List { start, items });
                }
                Event::Start(Tag::Table(aligns)) => {
                    let (mut head, mut rows) = (Vec::new(), Vec::new());
                    loop {
                        match self.events.next() {
                            Some(Event::Start(Tag::TableHead)) => head = self.cells(),
                            Some(Event::Start(Tag::TableRow)) => rows.push(self.cells()),
                            _ => break,
                        }
                    }
                    out.push(Block::Table { aligns, head, rows });
                }
                Event::Rule => out.push(Block::Rule),
                Event::Start(_) => transparent += 1,
                Event::End(_) if transparent > 0 => transparent -= 1,
                _ => break,
            }
        }
        out
    }

    /// The cells of one table row, up to and including the row's `End`.
    fn cells(&mut self) -> Vec<Vec<Span>> {
        let mut cells = Vec::new();
        while let Some(Event::Start(Tag::TableCell)) = self.events.next() {
            cells.push(self.inline(&mut None));
            self.end_tag();
        }
        cells
    }

    /// Reads inline content up to, and not including, the first event that is not
    /// inline: the `End` of the paragraph, or the next block in a tight list item.
    fn inline(&mut self, task: &mut Option<bool>) -> Vec<Span> {
        let mut state = InlineState::new();
        let mut open = 0usize;
        while let Some(event) = self.events.next_if(|event| is_inline(event, open)) {
            match event {
                Event::Start(tag) => {
                    open += 1;
                    match tag {
                        Tag::Emphasis => state.italic += 1,
                        Tag::Strong => state.bold += 1,
                        Tag::Strikethrough => state.strike += 1,
                        // `<a@b.c>` carries the bare address; the href is a mailto.
                        Tag::Link {
                            link_type: LinkType::Email,
                            dest_url,
                            ..
                        } => state.links.push(href(&format!("mailto:{dest_url}"))),
                        Tag::Link { dest_url, .. } => state.links.push(href(&dest_url)),
                        // An image is shown as its alt text, linked to the picture —
                        // unless it stands in a link, whose target a click on it opens.
                        Tag::Image { dest_url, .. } => {
                            let target = state.links.last().cloned();
                            state.links.push(target.unwrap_or_else(|| href(&dest_url)))
                        }
                        _ => {}
                    }
                }
                Event::End(tag) => {
                    open = open.saturating_sub(1);
                    match tag {
                        TagEnd::Emphasis => state.italic = state.italic.saturating_sub(1),
                        TagEnd::Strong => state.bold = state.bold.saturating_sub(1),
                        TagEnd::Strikethrough => state.strike = state.strike.saturating_sub(1),
                        TagEnd::Link | TagEnd::Image => {
                            state.links.pop();
                        }
                        _ => {}
                    }
                }
                Event::Text(text) => state.push_text(&text),
                Event::Code(text) => state.push(&text, true),
                // Raw HTML is shown as text; as text in HTML, its newlines are spaces.
                Event::Html(text) | Event::InlineHtml(text) => state.push(&text, false),
                // HTML collapses a newline inside a paragraph to a space.
                Event::SoftBreak => state.push(" ", false),
                Event::HardBreak => state.line_break(),
                Event::FootnoteReference(label) => {
                    let (number, target) = self.footnotes.reference(&label);
                    state.links.push(target);
                    state.push(&number.to_string(), false);
                    state.links.pop();
                }
                Event::TaskListMarker(checked) => {
                    task.get_or_insert(checked);
                }
                _ => {}
            }
        }
        // Whitespace at the end of a block is not rendered.
        while let Some(last) = state.spans.last_mut() {
            last.text.truncate(last.text.trim_end().len());
            if !last.text.is_empty() {
                break;
            }
            state.spans.pop();
        }
        loose_bold(state.spans)
    }
}

/// CommonMark does not close `**` that follows punctuation and precedes a letter, so
/// `**API(추정)**를` stays literal: common in Korean, where a particle follows the
/// closing mark. Such a pair left in a span's text is made bold here. A mark next to
/// a space on its inner side is still not one, as in CommonMark.
fn loose_bold(spans: Vec<Span>) -> Vec<Span> {
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        if span.code || span.bold || !span.text.contains("**") {
            out.push(span);
            continue;
        }
        let mut rest = span.text.as_str();
        let mut plain = String::new();
        while let Some(open) = rest.find("**") {
            let inner = &rest[open + 2..];
            let close = inner
                .match_indices("**")
                .map(|(at, _)| at)
                .find(|&at| at > 0 && !inner[..at].ends_with(char::is_whitespace));
            match close {
                Some(close) if !inner.starts_with(char::is_whitespace) => {
                    plain.push_str(&rest[..open]);
                    if !plain.is_empty() {
                        out.push(Span {
                            text: std::mem::take(&mut plain),
                            ..span.clone()
                        });
                    }
                    out.push(Span {
                        text: inner[..close].to_owned(),
                        bold: true,
                        ..span.clone()
                    });
                    rest = &inner[close + 2..];
                }
                _ => {
                    plain.push_str(&rest[..open + 2]);
                    rest = inner;
                }
            }
        }
        plain.push_str(rest);
        if !plain.is_empty() {
            out.push(Span {
                text: plain,
                ..span
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Measures of `.chat-log .md` in styles.css
// ---------------------------------------------------------------------------

/// `.chat-log { font-size: 14px }`; every `em` below is relative to it.
const FONT_PX: f32 = 14.0;
const LINE_HEIGHT: f32 = 1.65;
const HEADING_LINE_HEIGHT: f32 = 1.35;
const CODE_FONT_PX: f32 = 12.5;
const CODE_LINE_HEIGHT: f32 = 1.55;
const TABLE_FONT_PX: f32 = 13.0;
const LIST_INDENT_PX: f32 = 1.3 * FONT_PX;
const LIST_ITEM_MARGIN_PX: f32 = 0.3 * FONT_PX;
/// A table narrower than this per column scrolls sideways instead of squeezing.
const TABLE_MIN_COLUMN_PX: f32 = 72.0;
/// `setTimeout(() => setCopied(false), 1200)`.
const COPIED_MS: u64 = 1200;
/// Stands in for the CSS `ui-monospace, "SF Mono", Menlo, monospace`.
const MONO: &str = "Menlo";
const CODE_GROUP: &str = "md-code";

/// h1–h4 are styled by styles.css; h5 and h6 keep the browser's defaults.
fn heading_font_px(level: u8) -> f32 {
    FONT_PX
        * match level {
            1 => 1.28,
            2 => 1.16,
            3 => 1.04,
            4 => 1.0,
            5 => 0.83,
            _ => 0.67,
        }
}

/// The `line-height: 1.35` rule names h1–h4 only; h5 and h6 inherit the body's.
fn heading_line_height(level: u8) -> f32 {
    if level <= 4 {
        HEADING_LINE_HEIGHT
    } else {
        LINE_HEIGHT
    }
}

/// (margin-top, margin-bottom) of a block, in pixels.
fn margins(block: &Block) -> (f32, f32) {
    match block {
        Block::Paragraph(_) => (0.0, 0.75 * FONT_PX),
        Block::Text(_) => (0.0, 0.0),
        Block::Heading { level, .. } => {
            let size = heading_font_px(*level);
            match level {
                1..=4 => (1.2 * size, 0.5 * size),
                5 => (1.67 * size, 1.67 * size),
                _ => (2.33 * size, 2.33 * size),
            }
        }
        // The margin of <pre> is in its own, smaller font size.
        Block::Code(_) => (0.6 * CODE_FONT_PX, 0.9 * CODE_FONT_PX),
        Block::List { .. } => (0.35 * FONT_PX, 0.8 * FONT_PX),
        Block::Quote(_) | Block::Table { .. } => (0.6 * FONT_PX, 0.9 * FONT_PX),
        Block::Rule => (0.8 * FONT_PX, 0.8 * FONT_PX),
    }
}

/// The vertical space around a run of sibling blocks: before the first, between
/// each pair, after the last — one more entry than there are blocks. GPUI has no
/// margin collapsing, so the CSS result is computed here: two margins that meet take
/// the larger, and `p:last-child` has no bottom margin.
pub fn spacing(blocks: &[Block]) -> Vec<f32> {
    let mut out = Vec::with_capacity(blocks.len() + 1);
    let mut previous_bottom = 0.0f32;
    for (index, block) in blocks.iter().enumerate() {
        let (top, bottom) = margins(block);
        out.push(top.max(previous_bottom));
        let last = index + 1 == blocks.len();
        previous_bottom = if last && matches!(block, Block::Paragraph(_)) {
            0.0
        } else {
            bottom
        };
    }
    out.push(previous_bottom);
    out
}

/// The space above each list item. An `<li>` has no padding, so the margins of its
/// first and last block collapse through it into the gap between items.
fn item_spacing(items: &[ListItem]) -> Vec<f32> {
    let mut previous_bottom = 0.0f32;
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let inner = spacing(&item.blocks);
            let top = inner.first().copied().unwrap_or(0.0);
            let gap = if index == 0 {
                0.0
            } else {
                LIST_ITEM_MARGIN_PX.max(top).max(previous_bottom)
            };
            previous_bottom = inner.last().copied().unwrap_or(0.0);
            gap
        })
        .collect()
}

/// The marker of a list item: the number for an ordered list, else the browser's
/// disc, circle, square by nesting depth.
fn list_marker(start: Option<u64>, index: usize, depth: usize) -> String {
    match (start, depth) {
        (Some(start), _) => format!("{}.", start.saturating_add(index as u64)),
        (None, 0) => "•".into(),
        (None, 1) => "◦".into(),
        (None, _) => "▪".into(),
    }
}

/// Code as a `<pre>` shows it: the newline that ends the last line is not drawn, and
/// a tab runs to the next multiple of eight columns (the CSS default `tab-size`),
/// which GPUI's text layout does not do.
fn pre_text(code: &str) -> String {
    let code = code.strip_suffix('\n').unwrap_or(code);
    let mut out = String::with_capacity(code.len());
    let mut column = 0usize;
    for c in code.chars() {
        match c {
            '\t' => {
                let width = 8 - column % 8;
                out.extend(std::iter::repeat_n(' ', width));
                column += width;
            }
            '\n' => {
                out.push(c);
                column = 0;
            }
            _ => {
                out.push(c);
                column += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Elements
// ---------------------------------------------------------------------------

type LinkHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The styles.css tokens the kit's theme does not carry.
struct Tokens {
    link: Rgba,
    code_bg: Rgba,
    inline_code_bg: Rgba,
    accent_border: Rgba,
    surface: Rgba,
}

impl Tokens {
    fn of(theme: &Theme) -> Self {
        // The kit decides light or dark; this only asks which one it picked.
        if theme.bg == Theme::dark().bg {
            Self {
                link: rgb(0x7ab7ff),
                code_bg: rgb(0x0c0c0c),
                inline_code_bg: rgb(0x1d1d1d),
                accent_border: rgba(0x78aaff59),
                surface: rgb(0x1a1a1a),
            }
        } else {
            Self {
                link: rgb(0x2563cd),
                code_bg: rgb(0xf3f4f6),
                inline_code_bg: rgb(0xeceef1),
                accent_border: rgba(0x2f6fed59),
                surface: rgb(0xffffff),
            }
        }
    }
}

/// A chat message rendered as Markdown. Parsing happens here, on every call; it is
/// linear in the text and cheap enough to repeat while a message streams.
///
/// Several of these under one parent need an [`Markdown::id`] each (or an id on a
/// wrapper), so that code-block scroll positions are not shared between messages.
pub fn markdown(text: &str, theme: &Theme) -> Markdown {
    Markdown {
        id: "md".into(),
        blocks: parse(text),
        theme: *theme,
        dim: false,
        lang: Lang::En,
        on_link: None,
        selectable: false,
    }
}

/// One run of text rendered as-is: no Markdown parsing, nothing restyled.
/// For bubbles whose exact characters must survive (a user's own message),
/// with the same `.selectable(true)` support as [`markdown`].
pub fn plain(text: &str, theme: &Theme) -> Markdown {
    Markdown {
        id: "md".into(),
        blocks: vec![Block::Text(vec![Span {
            text: text.to_owned(),
            ..Span::default()
        }])],
        theme: *theme,
        dim: false,
        lang: Lang::En,
        on_link: None,
        selectable: false,
    }
}

#[derive(IntoElement)]
pub struct Markdown {
    id: ElementId,
    blocks: Vec<Block>,
    theme: Theme,
    dim: bool,
    lang: Lang,
    on_link: Option<LinkHandler>,
    selectable: bool,
}

impl Markdown {
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = id.into();
        self
    }

    /// `.md--dim`: faded and italic.
    pub fn dim(mut self, dim: bool) -> Self {
        self.dim = dim;
        self
    }

    /// Drag-to-select the text, with a copy bubble at the selection's head.
    /// Off by default: only panels that want selection pay for the hit maps.
    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    /// The language of the copy button.
    pub fn lang(mut self, lang: Lang) -> Self {
        self.lang = lang;
        self
    }

    /// Called with the URL when a link is clicked. Nothing is opened here; without a
    /// handler links are only colored.
    pub fn on_link(mut self, handler: impl Fn(&str, &mut Window, &mut App) + 'static) -> Self {
        self.on_link = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Markdown {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Text blocks record their layouts under this key when selection is on.
        let owner = self.selectable.then(|| selection::key(&self.id));
        let mut painter = Painter {
            theme: self.theme,
            tokens: Tokens::of(&self.theme),
            // Runs name their font, so the family the parent set is read here.
            font: window.text_style().font(),
            dim: self.dim,
            lang: self.lang,
            on_link: self.on_link,
            owner,
            next_id: 0,
        };
        let context = Context {
            color: self.theme.text,
            list_depth: 0,
        };
        let gaps = spacing(&self.blocks);
        let trailing = gaps.last().copied().unwrap_or(0.0);
        let root = div()
            .id(self.id)
            .flex()
            .flex_col()
            .w_full()
            .text_size(px(FONT_PX))
            .line_height(relative(LINE_HEIGHT))
            .text_color(self.theme.text)
            .when(self.dim, |root| root.opacity(0.6).italic());
        match owner {
            Some(owner) => {
                // Drop last frame's hit entries before any block records
                // new ones. This must run in PREPAINT: the whole tree
                // prepaints before anything paints, so a paint-phase clear
                // would wipe what this frame's own prepaint just recorded.
                let marker = div().absolute().child(canvas(
                    move |_, _, _cx| selection::clear_owner(owner),
                    move |_, _, _, _| {},
                ));
                let overlay = selection_overlay(owner);
                // Keyboard focus rides on the root so the app can bind Cmd-C
                // to a context only this element carries; a press on the text
                // moves focus here, and the input's own copy binding keeps
                // winning whenever the input is what holds focus.
                let focus = selection::focus_handle(cx, owner);
                let focus_for_down = focus.clone();
                root.child(marker)
                    // `.chat-log .md > :first-child { margin-top: 0 }`
                    .children(painter.stack(&self.blocks, &gaps, false, &context))
                    .pb(px(trailing))
                    .child(overlay)
                    .track_focus(&focus)
                    .key_context("MarkdownSelection")
                    .cursor(CursorStyle::IBeam)
                    .on_action(move |_: &CopySelection, window, cx| {
                        if selection::copy_and_clear(cx, owner) {
                            window.refresh();
                        }
                    })
                    .on_mouse_down(MouseButton::Left, move |event, window, _cx| {
                        if selection::mouse_down(owner, event) {
                            focus_for_down.focus(window);
                        } else {
                            window.refresh();
                        }
                    })
                    .on_mouse_move(move |event, window, _cx| {
                        if selection::mouse_move(owner, event) {
                            window.refresh();
                        }
                    })
                    .on_mouse_up(MouseButton::Left, move |event, window, _cx| {
                        if selection::mouse_up(owner, event) {
                            window.refresh();
                        }
                    })
            }
            None => root
                // `.chat-log .md > :first-child { margin-top: 0 }`
                .children(painter.stack(&self.blocks, &gaps, false, &context))
                .pb(px(trailing)),
        }
    }
}

/// The selection layer: a canvas that paints the highlight over the text.
/// It has no hitbox of its own, so mouse events pass through to the text.
fn selection_overlay(owner: u64) -> Div {
    div().absolute().size_full().child(canvas(
        move |_, _, _| {},
        move |_, _, window, _cx| {
            for rect in selection::highlight(owner) {
                window.paint_quad(gpui::fill(rect, selection::highlight_color()));
            }
        },
    ))
}

/// What a block inherits from the blocks around it.
#[derive(Clone, Copy)]
struct Context {
    color: Rgba,
    list_depth: usize,
}

struct Painter {
    theme: Theme,
    tokens: Tokens,
    font: Font,
    dim: bool,
    lang: Lang,
    on_link: Option<LinkHandler>,
    /// Set when the markdown root has selection on: text blocks record their
    /// layouts under this key.
    owner: Option<u64>,
    next_id: usize,
}

impl Painter {
    /// Ids count up in document order, so a block keeps its id while text is appended.
    fn id(&mut self, name: &'static str) -> ElementId {
        self.next_id += 1;
        (name, self.next_id).into()
    }

    /// Sibling blocks, each under its collapsed margin. `leading` keeps the first
    /// block's own top margin.
    fn stack(
        &mut self,
        blocks: &[Block],
        gaps: &[f32],
        leading: bool,
        context: &Context,
    ) -> Vec<AnyElement> {
        blocks
            .iter()
            .zip(gaps)
            .enumerate()
            .map(|(index, (block, gap))| {
                let gap = if index == 0 && !leading { 0.0 } else { *gap };
                div()
                    .mt(px(gap))
                    .child(self.block(block, context))
                    .into_any_element()
            })
            .collect()
    }

    fn block(&mut self, block: &Block, context: &Context) -> AnyElement {
        match block {
            Block::Paragraph(spans) | Block::Text(spans) => {
                self.text(spans, context.color, FontWeight::NORMAL)
            }
            Block::Heading { level, content } => div()
                .text_size(px(heading_font_px(*level)))
                .line_height(relative(heading_line_height(*level)))
                .child(self.text(
                    content,
                    context.color,
                    if *level <= 4 {
                        FontWeight(650.0)
                    } else {
                        FontWeight::BOLD
                    },
                ))
                .into_any_element(),
            Block::Code(code) => self.code_block(code, context),
            Block::List { start, items } => self.list(*start, items, context),
            Block::Quote(blocks) => {
                let inner = Context {
                    color: self.theme.text_2,
                    ..*context
                };
                let gaps = spacing(blocks);
                // The quote's padding keeps its children's outer margins inside it.
                let trailing = gaps.last().copied().unwrap_or(0.0);
                div()
                    .flex()
                    .flex_col()
                    .border_l(px(3.0))
                    .border_color(self.tokens.accent_border)
                    .pl(px(0.9 * FONT_PX))
                    .pt(px(0.2 * FONT_PX))
                    .pb(px(0.2 * FONT_PX + trailing))
                    .text_color(self.theme.text_2)
                    .children(self.stack(blocks, &gaps, true, &inner))
                    .into_any_element()
            }
            Block::Table { aligns, head, rows } => self.table(aligns, head, rows, context),
            Block::Rule => div().h(px(1.0)).bg(self.theme.border).into_any_element(),
        }
    }

    /// One paragraph: a single wrapped text whose runs carry the inline styles.
    fn text(&mut self, spans: &[Span], color: Rgba, weight: FontWeight) -> AnyElement {
        let mut text = String::new();
        let mut runs = Vec::with_capacity(spans.len());
        let mut links: Vec<(Range<usize>, String)> = Vec::new();
        for span in spans {
            let start = text.len();
            text.push_str(&span.text);
            let mut run_font = if span.code {
                font(MONO)
            } else {
                self.font.clone()
            };
            run_font.weight = if span.bold { FontWeight::BOLD } else { weight };
            run_font.style = if span.italic || self.dim {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            };
            let color = if span.link.is_some() {
                self.tokens.link
            } else {
                color
            };
            runs.push(TextRun {
                len: span.text.len(),
                font: run_font,
                color: color.into(),
                background_color: span.code.then(|| Hsla::from(self.tokens.inline_code_bg)),
                underline: None,
                strikethrough: span.strike.then(|| StrikethroughStyle {
                    thickness: px(1.0),
                    color: None,
                }),
            });
            if let Some(url) = span.link.as_ref().filter(|url| !url.is_empty()) {
                links.push((start..text.len(), url.clone()));
            }
        }
        let styled = StyledText::new(text.clone()).with_runs(runs);
        // The layout handle is cloned before the styled text moves on: the
        // clone shares the same measurement cell, so it fills during prepaint.
        let layout = styled.layout().clone();
        // Counted even without links, so ordinals stay unique per block.
        self.next_id += 1;
        let ordinal = self.next_id;
        let inner = match &self.on_link {
            Some(on_link) if !links.is_empty() => {
                let on_link = on_link.clone();
                let (ranges, urls): (Vec<_>, Vec<_>) = links.into_iter().unzip();
                InteractiveText::new(self.id("md-text"), styled)
                    .on_click(ranges, move |index, window, cx| {
                        if let Some(url) = urls.get(index) {
                            on_link(url, window, cx);
                        }
                    })
                    .into_any_element()
            }
            _ => styled.into_any_element(),
        };
        match self.owner {
            Some(owner) => selection::SelectableText::new(
                owner,
                ordinal,
                layout,
                SharedString::from(text),
                inner,
            )
            .into_any_element(),
            None => inner,
        }
    }

    /// `<div class="code-block">`: the `<pre>` scrolls sideways, and the copy button
    /// appears on hover so it never sits on top of the code.
    fn code_block(&mut self, code: &str, context: &Context) -> AnyElement {
        let shown = pre_text(code);
        div()
            .id(self.id("md-code"))
            .group(CODE_GROUP)
            .relative()
            .child(
                div()
                    .id("pre")
                    .overflow_x_scroll()
                    .bg(self.tokens.code_bg)
                    .border_1()
                    .border_color(self.theme.border)
                    .rounded(px(8.0))
                    .px(px(14.0))
                    .py(px(12.0))
                    .font_family(MONO)
                    .text_size(px(CODE_FONT_PX))
                    .line_height(relative(CODE_LINE_HEIGHT))
                    .text_color(context.color)
                    .whitespace_nowrap()
                    .child(SharedString::from(shown)),
            )
            .child(CopyButton {
                // `innerText` of the <pre>: the code with its final newline.
                code: code.to_owned(),
                lang: self.lang,
                background: self.tokens.surface,
                border: self.theme.border_2,
                color: self.theme.text_2,
            })
            .into_any_element()
    }

    fn list(&mut self, start: Option<u64>, items: &[ListItem], context: &Context) -> AnyElement {
        let inner = Context {
            list_depth: context.list_depth + 1,
            ..*context
        };
        let gaps = item_spacing(items);
        let rows = items
            .iter()
            .zip(gaps)
            .enumerate()
            .map(|(index, (item, gap))| {
                let marker = list_marker(start, index, context.list_depth);
                div()
                    .flex()
                    .items_start()
                    .mt(px(gap))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .justify_end()
                            .min_w(px(LIST_INDENT_PX))
                            .pr(px(5.0))
                            // `.chat-log .md li::marker`
                            .text_color(self.theme.text_dim)
                            .child(marker),
                    )
                    .when_some(item.task, |row, checked| row.child(self.checkbox(checked)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .children(self.stack(
                                &item.blocks,
                                &spacing(&item.blocks),
                                false,
                                &inner,
                            )),
                    )
            });
        div()
            .flex()
            .flex_col()
            .children(rows.collect::<Vec<_>>())
            .into_any_element()
    }

    /// The disabled `<input type="checkbox">` remark-gfm puts in a task item.
    fn checkbox(&self, checked: bool) -> impl IntoElement + use<> {
        const SIZE: f32 = 13.0;
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(SIZE))
            // Centered on the first line of the item.
            .mt(px((FONT_PX * LINE_HEIGHT - SIZE) / 2.0))
            .mr(px(6.0))
            .rounded(px(3.0))
            .border_1()
            .border_color(self.theme.border_2)
            .text_size(px(10.0))
            .line_height(px(SIZE))
            .when(checked, |checkbox| {
                checkbox
                    .bg(self.theme.accent)
                    .border_color(self.theme.accent)
                    .text_color(rgb(0xffffff))
                    .child("✓")
            })
    }

    /// `<div class="md-table">`: wide tables scroll inside their own box instead of
    /// running past the panel edge.
    fn table(
        &mut self,
        aligns: &[Alignment],
        head: &[Vec<Span>],
        rows: &[Vec<Vec<Span>>],
        context: &Context,
    ) -> AnyElement {
        let columns = head.len().max(1);
        // Equal columns that share the box's width. Tracks sized by their content, as a
        // browser sizes table columns, came out wider than the panel: GPUI measures a
        // text's narrowest width as its unwrapped width, so the cells never wrapped
        // and the table ran past the edge. Cells are placed explicitly.
        let mut grid = div()
            .grid()
            .grid_cols(columns as u16)
            .w_full()
            .min_w(px(columns as f32 * TABLE_MIN_COLUMN_PX));
        let lines = std::iter::once(head).chain(rows.iter().map(Vec::as_slice));
        for (row, cells) in lines.enumerate() {
            let is_head = row == 0;
            for (column, spans) in cells.iter().enumerate() {
                let (Ok(column_line), Ok(row_line)) =
                    (i16::try_from(column + 1), i16::try_from(row + 1))
                else {
                    continue;
                };
                let content = if is_head {
                    self.text(spans, self.theme.text_2, FontWeight::SEMIBOLD)
                } else {
                    self.text(spans, context.color, FontWeight::NORMAL)
                };
                let cell = div()
                    .col_start(column_line)
                    .row_start(row_line)
                    // Lets a column shrink below its unwrapped text, so cells wrap.
                    .min_w_0()
                    .px(px(10.0))
                    .py(px(7.0))
                    // `tr:last-child td { border-bottom: none }`
                    .when(is_head || row < rows.len(), |cell| {
                        cell.border_b_1().border_color(self.theme.border)
                    })
                    .when(is_head, |cell| cell.bg(self.theme.surface_2))
                    // The clip of the rounded box is square; round the corners here.
                    .when(is_head && column == 0, |cell| cell.rounded_tl(px(7.0)))
                    .when(is_head && column + 1 == columns, |cell| {
                        cell.rounded_tr(px(7.0))
                    })
                    .map(|cell| match aligns.get(column) {
                        Some(Alignment::Center) => cell.text_center(),
                        Some(Alignment::Right) => cell.text_right(),
                        _ => cell,
                    })
                    .child(content);
                grid = grid.child(cell);
            }
        }
        div()
            .id(self.id("md-table"))
            .overflow_x_scroll()
            .border_1()
            .border_color(self.theme.border)
            .rounded(px(8.0))
            .text_size(px(TABLE_FONT_PX))
            .child(grid)
            .into_any_element()
    }
}

/// `.code-copy`. Its own component so the "copied" flag is element state under the
/// code block's id.
#[derive(IntoElement)]
struct CopyButton {
    code: String,
    lang: Lang,
    background: Rgba,
    border: Rgba,
    color: Rgba,
}

impl RenderOnce for CopyButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let copied = window.use_keyed_state("copied", cx, |_, _| false);
        let label = self.lang.t(if *copied.read(cx) {
            "md.copied"
        } else {
            "md.copy"
        });
        let code = self.code;
        div()
            .id("copy")
            .absolute()
            .top(px(6.0))
            .right(px(6.0))
            .px(px(8.0))
            .py(px(2.0))
            .text_size(px(11.0))
            .bg(self.background)
            .border_1()
            .border_color(self.border)
            .text_color(self.color)
            .rounded(px(6.0))
            .cursor_pointer()
            .opacity(0.0)
            .group_hover(CODE_GROUP, |style| style.opacity(1.0))
            .child(label)
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                copied.update(cx, |copied, cx| {
                    *copied = true;
                    cx.notify();
                });
                let copied = copied.downgrade();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(COPIED_MS))
                        .await;
                    let _ = copied.update(cx, |copied, cx| {
                        *copied = false;
                        cx.notify();
                    });
                })
                .detach();
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Span {
        Span {
            text: text.into(),
            ..Span::default()
        }
    }

    fn link(text: &str, url: &str) -> Span {
        Span {
            link: Some(url.into()),
            ..plain(text)
        }
    }

    #[test]
    fn 빈_문자열은_블록이_없다() {
        assert_eq!(parse(""), vec![]);
        assert_eq!(parse("  \n\n "), vec![]);
    }

    #[test]
    fn 문단은_빈_줄로_나뉘고_줄바꿈은_공백이_된다() {
        assert_eq!(
            parse("첫 줄\n둘째 줄\n\n다음 문단"),
            vec![
                Block::Paragraph(vec![plain("첫 줄 둘째 줄")]),
                Block::Paragraph(vec![plain("다음 문단")]),
            ]
        );
    }

    #[test]
    fn 줄_끝_공백_두_칸은_강제_줄바꿈이다() {
        assert_eq!(parse("a  \nb"), vec![Block::Paragraph(vec![plain("a\nb")])]);
    }

    #[test]
    fn 굵게_기울임_취소선_인라인_코드를_구간으로_나눈다() {
        assert_eq!(
            parse("a **b** *c* ~~d~~ `e` ***f***"),
            vec![Block::Paragraph(vec![
                plain("a "),
                Span {
                    bold: true,
                    ..plain("b")
                },
                plain(" "),
                Span {
                    italic: true,
                    ..plain("c")
                },
                plain(" "),
                Span {
                    strike: true,
                    ..plain("d")
                },
                plain(" "),
                Span {
                    code: true,
                    ..plain("e")
                },
                plain(" "),
                Span {
                    bold: true,
                    italic: true,
                    ..plain("f")
                },
            ])]
        );
    }

    #[test]
    fn 링크는_주소를_담고_안쪽_강조를_유지한다() {
        assert_eq!(
            parse("[문서 **보기**](https://example.com/a) 끝"),
            vec![Block::Paragraph(vec![
                link("문서 ", "https://example.com/a"),
                Span {
                    bold: true,
                    ..link("보기", "https://example.com/a")
                },
                plain(" 끝"),
            ])]
        );
    }

    #[test]
    fn 이미지는_대체_텍스트를_그림_주소로_잇는다() {
        assert_eq!(
            parse("![그림](https://example.com/a.png)"),
            vec![Block::Paragraph(vec![link(
                "그림",
                "https://example.com/a.png"
            )])]
        );
    }

    #[test]
    fn 위험한_프로토콜의_링크는_주소가_비워진다() {
        assert_eq!(
            parse("[x](javascript:alert(1))"),
            vec![Block::Paragraph(vec![link("x", "")])]
        );
        assert_eq!(safe_url("javascript:alert(1)"), "");
        assert_eq!(safe_url("data:text/html,x"), "");
        assert_eq!(safe_url("HTTPS://a.b"), "HTTPS://a.b");
        assert_eq!(safe_url("mailto:a@b.c"), "mailto:a@b.c");
        // 프로토콜이 없으면 상대 주소다.
        assert_eq!(safe_url("/path?x=a:b"), "/path?x=a:b");
        assert_eq!(safe_url("page#a:b"), "page#a:b");
        assert_eq!(safe_url("?q=a:b"), "?q=a:b");
        assert_eq!(safe_url(""), "");
    }

    #[test]
    fn 맨_주소는_자동으로_링크가_된다() {
        assert_eq!(
            parse("보기: https://example.com/a_b?x=1. 그리고 (www.example.com)"),
            vec![Block::Paragraph(vec![
                plain("보기: "),
                link("https://example.com/a_b?x=1", "https://example.com/a_b?x=1"),
                plain(". 그리고 ("),
                link("www.example.com", "http://www.example.com"),
                plain(")"),
            ])]
        );
    }

    #[test]
    fn 자동_링크는_단어_중간이나_주소가_없으면_만들지_않는다() {
        assert_eq!(find_autolink("xhttps://example.com", None), None);
        assert_eq!(find_autolink("https:// 뒤가 없다", None), None);
        assert_eq!(find_autolink("www", None), None);
        // remark-gfm의 mdast 단계는 "www."에서 "www"를 링크로 만든다.
        assert_eq!(
            find_autolink("www.", None),
            Some((0, 3, "http://www".into()))
        );
        // 괄호가 짝이 맞으면 주소의 일부다.
        assert_eq!(
            find_autolink("https://a.b/c_(d)", None),
            Some((0, 17, "https://a.b/c_(d)".into()))
        );
        // 이미 링크인 글자는 다시 링크로 만들지 않는다.
        assert_eq!(
            parse("[https://a.b](https://c.d)"),
            vec![Block::Paragraph(vec![link("https://a.b", "https://c.d")])]
        );
        // 코드 안의 주소는 코드다.
        assert_eq!(
            parse("`https://a.b`"),
            vec![Block::Paragraph(vec![Span {
                code: true,
                ..plain("https://a.b")
            }])]
        );
    }

    #[test]
    fn 제목은_단계와_함께_나온다() {
        assert_eq!(
            parse("# 하나\n## 둘\n###### 여섯"),
            vec![
                Block::Heading {
                    level: 1,
                    content: vec![plain("하나")]
                },
                Block::Heading {
                    level: 2,
                    content: vec![plain("둘")]
                },
                Block::Heading {
                    level: 6,
                    content: vec![plain("여섯")]
                },
            ]
        );
    }

    #[test]
    fn 코드_블록은_본문을_그대로_둔다() {
        assert_eq!(
            parse("```rust\nfn main() {\n    let a = \"**x**\";\n}\n```"),
            vec![Block::Code(
                "fn main() {\n    let a = \"**x**\";\n}\n".into()
            )]
        );
        // 들여쓰기 코드 블록.
        assert_eq!(parse("    a\n    b"), vec![Block::Code("a\nb".into())]);
    }

    #[test]
    fn 닫히지_않은_코드_펜스는_끝까지_코드다() {
        assert_eq!(
            parse("앞\n\n```js\nconst a = 1"),
            vec![
                Block::Paragraph(vec![plain("앞")]),
                Block::Code("const a = 1".into()),
            ]
        );
        assert_eq!(parse("```"), vec![Block::Code(String::new())]);
    }

    #[test]
    fn 스트리밍_중_잘린_입력은_어디서_끊겨도_패닉하지_않는다() {
        let sample = "# 제목\n\n문단 **굵게** `코드` [링크](https://a.b) ~~취소~~\n\n\
            - [ ] 할 일\n  1. 안쪽\n- [x] 끝\n\n> 인용\n> > 안쪽 인용\n\n\
            | a | b |\n|:-:|--:|\n| 1 | 2 |\n\n```rs\nlet x = 1;\n```\n\n---\n<div>html</div>\n";
        for end in 0..=sample.len() {
            if let Some(prefix) = sample.get(..end) {
                // 글자 경계마다 잘라, 덜 닫힌 강조·링크·표·펜스 어디서도 버티는지 본다.
                let _ = spacing(&parse(prefix));
            }
        }
    }

    #[test]
    fn 촘촘한_목록의_항목은_문단_없이_글자만_담는다() {
        assert_eq!(
            parse("- 하나\n- 둘"),
            vec![Block::List {
                start: None,
                items: vec![
                    ListItem {
                        task: None,
                        blocks: vec![Block::Text(vec![plain("하나")])]
                    },
                    ListItem {
                        task: None,
                        blocks: vec![Block::Text(vec![plain("둘")])]
                    },
                ],
            }]
        );
    }

    #[test]
    fn 느슨한_목록의_항목은_문단을_담는다() {
        assert_eq!(
            parse("- 하나\n\n- 둘"),
            vec![Block::List {
                start: None,
                items: vec![
                    ListItem {
                        task: None,
                        blocks: vec![Block::Paragraph(vec![plain("하나")])]
                    },
                    ListItem {
                        task: None,
                        blocks: vec![Block::Paragraph(vec![plain("둘")])]
                    },
                ],
            }]
        );
    }

    #[test]
    fn 번호_목록은_시작_번호를_가지고_안쪽_목록을_품는다() {
        assert_eq!(
            parse("3. 셋\n   - 안쪽\n4. 넷"),
            vec![Block::List {
                start: Some(3),
                items: vec![
                    ListItem {
                        task: None,
                        blocks: vec![
                            Block::Text(vec![plain("셋")]),
                            Block::List {
                                start: None,
                                items: vec![ListItem {
                                    task: None,
                                    blocks: vec![Block::Text(vec![plain("안쪽")])]
                                }],
                            },
                        ],
                    },
                    ListItem {
                        task: None,
                        blocks: vec![Block::Text(vec![plain("넷")])]
                    },
                ],
            }]
        );
    }

    #[test]
    fn 할_일_항목은_체크_여부를_가지고_안쪽_항목과_섞이지_않는다() {
        let blocks = parse("- [ ] 바깥\n  - [x] 안쪽\n- 보통\n\n- [x] 느슨한 항목");
        let Some(Block::List { items, .. }) = blocks.first() else {
            panic!("목록이 아니다: {blocks:?}");
        };
        assert_eq!(
            items.iter().map(|item| item.task).collect::<Vec<_>>(),
            vec![Some(false), None, Some(true)]
        );
        let Some(Block::List { items: inner, .. }) = items[0].blocks.get(1) else {
            panic!("안쪽 목록이 없다: {:?}", items[0].blocks);
        };
        assert_eq!(inner[0].task, Some(true));
        assert_eq!(inner[0].blocks, vec![Block::Text(vec![plain("안쪽")])]);
    }

    #[test]
    fn 인용은_안의_블록을_품는다() {
        assert_eq!(
            parse("> 인용\n>\n> - 항목"),
            vec![Block::Quote(vec![
                Block::Paragraph(vec![plain("인용")]),
                Block::List {
                    start: None,
                    items: vec![ListItem {
                        task: None,
                        blocks: vec![Block::Text(vec![plain("항목")])]
                    }],
                },
            ])]
        );
    }

    #[test]
    fn 표는_정렬과_머리와_행을_가진다() {
        assert_eq!(
            parse("| 이름 | 값 | 비고 |\n|:--|:-:|--:|\n| a | `1` | |\n| b | 2 | **c** |"),
            vec![Block::Table {
                aligns: vec![Alignment::Left, Alignment::Center, Alignment::Right],
                head: vec![vec![plain("이름")], vec![plain("값")], vec![plain("비고")]],
                rows: vec![
                    vec![
                        vec![plain("a")],
                        vec![Span {
                            code: true,
                            ..plain("1")
                        }],
                        vec![],
                    ],
                    vec![
                        vec![plain("b")],
                        vec![plain("2")],
                        vec![Span {
                            bold: true,
                            ..plain("c")
                        }],
                    ],
                ],
            }]
        );
    }

    #[test]
    fn 구분선은_블록_하나다() {
        assert_eq!(
            parse("위\n\n---\n\n아래"),
            vec![
                Block::Paragraph(vec![plain("위")]),
                Block::Rule,
                Block::Paragraph(vec![plain("아래")]),
            ]
        );
    }

    #[test]
    fn 날_html은_실행되지_않고_글자로_보인다() {
        assert_eq!(
            parse("<div onclick=\"x()\">\n안녕\n</div>"),
            vec![Block::Text(vec![plain(
                "<div onclick=\"x()\"> 안녕 </div>"
            )])]
        );
        assert_eq!(
            parse("앞 <b>굵게</b> 뒤"),
            vec![Block::Paragraph(vec![plain("앞 <b>굵게</b> 뒤")])]
        );
    }

    #[test]
    fn 지나치게_깊은_중첩은_평평하게_펴서_스택을_지킨다() {
        fn depth(blocks: &[Block]) -> usize {
            blocks
                .iter()
                .map(|block| match block {
                    Block::Quote(inner) => 1 + depth(inner),
                    Block::List { items, .. } => {
                        1 + items
                            .iter()
                            .map(|item| depth(&item.blocks))
                            .max()
                            .unwrap_or(0)
                    }
                    _ => 0,
                })
                .max()
                .unwrap_or(0)
        }
        let quotes = format!("{} 바닥", ">".repeat(5000));
        let blocks = parse(&quotes);
        assert_eq!(depth(&blocks), MAX_DEPTH);
        // 펴진 뒤에도 글자는 남는다.
        assert!(format!("{blocks:?}").contains("바닥"));

        let lists: String = (0..200)
            .map(|level| format!("{}- {level}\n", "  ".repeat(level)))
            .collect();
        assert!(depth(&parse(&lists)) <= MAX_DEPTH);
    }

    #[test]
    fn 구간_길이의_합은_본문_길이와_같다() {
        // with_runs는 길이가 어긋나면 패닉하므로, 한글·이모지에서도 바이트 길이가 맞아야 한다.
        let blocks = parse("한글 **굵게** 😀 `코드` https://example.com/가 끝");
        let Some(Block::Paragraph(spans)) = blocks.first() else {
            panic!("문단이 아니다: {blocks:?}");
        };
        let joined: String = spans.iter().map(|span| span.text.as_str()).collect();
        assert_eq!(joined, "한글 굵게 😀 코드 https://example.com/가 끝");
        assert!(spans.iter().all(|span| !span.text.is_empty()));
    }

    #[test]
    fn 맞닿은_여백은_큰_쪽_하나만_남는다() {
        let blocks = parse("문단\n\n# 제목\n\n문단\n\n- 항목\n\n마지막");
        let h1 = heading_font_px(1);
        assert_eq!(
            spacing(&blocks),
            vec![
                0.0,
                // p 아래 0.75em 과 h1 위 1.2em 중 큰 쪽.
                1.2 * h1,
                0.5 * h1,
                // p 아래 0.75em 이 ul 위 0.35em 보다 크다.
                0.75 * FONT_PX,
                0.8 * FONT_PX,
                // p:last-child 는 아래 여백이 없다.
                0.0,
            ]
        );
        assert_eq!(spacing(&[]), vec![0.0]);
    }

    #[test]
    fn 마지막이_문단이_아니면_아래_여백이_남는다() {
        let blocks = parse("문단\n\n```\ncode\n```");
        assert_eq!(
            spacing(&blocks),
            vec![0.0, 0.75 * FONT_PX, 0.9 * CODE_FONT_PX]
        );
    }

    #[test]
    fn 목록_항목_사이는_li_여백이고_안쪽_목록의_아래_여백이_새어_나온다() {
        let blocks = parse("- 하나\n- 둘\n  - 안쪽\n- 셋");
        let Some(Block::List { items, .. }) = blocks.first() else {
            panic!("목록이 아니다: {blocks:?}");
        };
        assert_eq!(
            item_spacing(items),
            vec![0.0, LIST_ITEM_MARGIN_PX, 0.8 * FONT_PX]
        );
    }

    #[test]
    fn 목록_표시는_번호이거나_깊이에_따른_점이다() {
        assert_eq!(list_marker(Some(3), 0, 0), "3.");
        assert_eq!(list_marker(Some(3), 2, 4), "5.");
        assert_eq!(list_marker(Some(u64::MAX), 2, 0), format!("{}.", u64::MAX));
        assert_eq!(list_marker(None, 0, 0), "•");
        assert_eq!(list_marker(None, 5, 1), "◦");
        assert_eq!(list_marker(None, 0, 2), "▪");
    }

    #[test]
    fn 제목_글자_크기는_css_배율을_따른다() {
        assert_eq!(heading_font_px(1), 14.0 * 1.28);
        assert_eq!(heading_font_px(2), 14.0 * 1.16);
        assert_eq!(heading_font_px(3), 14.0 * 1.04);
        assert_eq!(heading_font_px(4), 14.0);
        assert!(heading_font_px(6) < heading_font_px(5));
        // 줄 높이 1.35는 h1~h4에만 걸려 있고, h5·h6은 본문의 1.65를 물려받는다.
        assert_eq!(heading_line_height(4), 1.35);
        assert_eq!(heading_line_height(5), 1.65);
        assert_eq!(heading_line_height(6), 1.65);
    }

    /// 문단 하나의 구간들. 아래 기대값은 react-markdown 10.1.0 + remark-gfm 4.0.1을
    /// bun으로 돌려 얻은 결과다.
    fn inline(text: &str) -> Vec<Span> {
        match parse(text).into_iter().next() {
            Some(Block::Paragraph(spans)) => spans,
            other => panic!("문단이 아니다: {other:?}"),
        }
    }

    #[test]
    fn 자동_링크는_글자_바로_뒤가_아니면_어디서든_시작한다() {
        assert_eq!(
            inline("\"https://example.com\" quoted"),
            vec![
                plain("\""),
                link("https://example.com", "https://example.com"),
                plain("\" quoted"),
            ]
        );
        // 한글과 숫자는 ASCII 글자가 아니라서 바로 붙어도 링크다.
        assert_eq!(
            inline("주소는https://example.com/a 끝"),
            vec![
                plain("주소는"),
                link("https://example.com/a", "https://example.com/a"),
                plain(" 끝"),
            ]
        );
        assert_eq!(
            inline("1https://example.com"),
            vec![
                plain("1"),
                link("https://example.com", "https://example.com")
            ]
        );
        assert_eq!(
            inline("xhttps://example.com"),
            vec![plain("xhttps://example.com")]
        );
        assert_eq!(
            inline("[https://example.com]"),
            vec![
                plain("["),
                link("https://example.com", "https://example.com"),
                plain("]"),
            ]
        );
        // www는 공백이나 ( * _ [ ] ~ 뒤, 또는 다른 문장부호 뒤에서 시작한다.
        assert_eq!(
            inline("\"www.example.com\""),
            vec![
                plain("\""),
                link("www.example.com", "http://www.example.com"),
                plain("\""),
            ]
        );
        assert_eq!(inline("xwww.example.com"), vec![plain("xwww.example.com")]);
        // 이모지는 UTF-16 한 단위로 보면 문장부호가 아니다.
        assert_eq!(
            inline("😀www.example.com"),
            vec![plain("😀www.example.com")]
        );
        assert_eq!(
            inline("😀https://example.com"),
            vec![
                plain("😀"),
                link("https://example.com", "https://example.com")
            ]
        );
    }

    #[test]
    fn 자동_링크의_프로토콜과_www는_대소문자를_가리지_않는다() {
        assert_eq!(
            inline("HTTPS://EXAMPLE.COM/A"),
            vec![link("HTTPS://EXAMPLE.COM/A", "HTTPS://EXAMPLE.COM/A")]
        );
        assert_eq!(
            inline("Www.example.com"),
            vec![link("Www.example.com", "http://Www.example.com")]
        );
        assert_eq!(
            inline("ftp://example.com/x"),
            vec![plain("ftp://example.com/x")]
        );
    }

    #[test]
    fn 자동_링크의_도메인은_점이_없어도_되지만_끝_두_마디에_밑줄이_없어야_한다() {
        assert_eq!(
            inline("http://localhost:3000/api"),
            vec![link(
                "http://localhost:3000/api",
                "http://localhost:3000/api"
            )]
        );
        assert_eq!(
            inline("http://127.0.0.1:8777/snapshot-fixture.html"),
            vec![link(
                "http://127.0.0.1:8777/snapshot-fixture.html",
                "http://127.0.0.1:8777/snapshot-fixture.html"
            )]
        );
        assert_eq!(inline("https://a"), vec![link("https://a", "https://a")]);
        for text in [
            "www.a_b.com",
            "https://a_b.c",
            "https://a.b_c",
            "https://a.b.c_d.e",
            "https://",
            "https:// x",
        ] {
            assert_eq!(inline(text), vec![plain(text)], "{text}");
        }
        // 앞쪽 마디의 밑줄은 괜찮다.
        assert_eq!(
            inline("www.x_y.a.com"),
            vec![link("www.x_y.a.com", "http://www.x_y.a.com")]
        );
        // 토큰화 단계가 거절해도 mdast 단계가 받아 주는 도메인.
        assert_eq!(
            inline("https://-a.b"),
            vec![link("https://-a.b", "https://-a.b")]
        );
    }

    #[test]
    fn 자동_링크_끝의_문장부호는_링크에서_빠진다() {
        for (text, url, tail) in [
            ("https://example.com/a;", "https://example.com/a", ";"),
            ("https://example.com/a]", "https://example.com/a", "]"),
            ("https://example.com/a*", "https://example.com/a", "*"),
            ("https://example.com/a...", "https://example.com/a", "..."),
            ("https://example.com/a!?", "https://example.com/a", "!?"),
            ("https://example.com/a))", "https://example.com/a", "))"),
            ("https://example.com/a<b", "https://example.com/a", "<b"),
            (
                "https://example.com:8080/a:",
                "https://example.com:8080/a",
                ":",
            ),
            ("https://example.com/a] x", "https://example.com/a", "] x"),
            ("https://example.com/a, b", "https://example.com/a", ", b"),
        ] {
            assert_eq!(inline(text), vec![link(url, url), plain(tail)], "{text}");
        }
        // 뒤에 주소가 이어지면 문장부호도 주소의 일부다.
        for text in [
            "https://example.com/a&b=c;d",
            "https://example.com/a,b",
            "https://example.com/a(b)c",
            "https://a.b/?x=(y)",
        ] {
            assert_eq!(inline(text), vec![link(text, text)], "{text}");
        }
        assert_eq!(
            inline("https://example.com/a]b"),
            vec![link("https://example.com/a]b", "https://example.com/a%5Db")]
        );
        // } 는 주소에 남고, href에서는 퍼센트 인코딩된다.
        assert_eq!(
            inline("https://example.com/a}"),
            vec![link("https://example.com/a}", "https://example.com/a%7D")]
        );
        assert_eq!(
            inline("(www.example.com/a_(b))"),
            vec![
                plain("("),
                link("www.example.com/a_(b)", "http://www.example.com/a_(b)"),
                plain(")"),
            ]
        );
    }

    #[test]
    fn 맨_이메일_주소는_mailto_링크가_된다() {
        assert_eq!(
            inline("mail a.b+c@example.co.kr."),
            vec![
                plain("mail "),
                link("a.b+c@example.co.kr", "mailto:a.b+c@example.co.kr"),
                plain("."),
            ]
        );
        assert_eq!(
            inline("메일은a@b.com입니다"),
            vec![
                plain("메일은"),
                link("a@b.com", "mailto:a@b.com"),
                plain("입니다"),
            ]
        );
        assert_eq!(
            inline("git@github.com:org/repo.git"),
            vec![
                link("git@github.com", "mailto:git@github.com"),
                plain(":org/repo.git"),
            ]
        );
        assert_eq!(
            inline("a@b.com,c@d.org"),
            vec![
                link("a@b.com", "mailto:a@b.com"),
                plain(","),
                link("c@d.org", "mailto:c@d.org"),
            ]
        );
        // 점이 없거나, 글자로 끝나지 않거나, / 바로 뒤면 주소가 아니다.
        for text in [
            "a@b",
            "a@b.c1",
            "a@b.c-",
            "/a@b.com",
            "react@18.2.0",
            "@b.c",
            "a@",
        ] {
            assert_eq!(inline(text), vec![plain(text)], "{text}");
        }
    }

    #[test]
    fn 꺾쇠_이메일_자동_링크의_주소는_mailto다() {
        assert_eq!(inline("<a@b.com>"), vec![link("a@b.com", "mailto:a@b.com")]);
        assert_eq!(
            inline("<mailto:a@b.com>"),
            vec![link("mailto:a@b.com", "mailto:a@b.com")]
        );
    }

    #[test]
    fn 링크_안의_이미지는_링크의_주소를_따른다() {
        assert_eq!(
            inline("[![alt](https://img/x.png)](https://link.example)"),
            vec![link("alt", "https://link.example")]
        );
    }

    #[test]
    fn 링크_주소는_퍼센트_인코딩된_뒤_프로토콜을_검사한다() {
        assert_eq!(
            normalize_uri("https://example.com/가"),
            "https://example.com/%EA%B0%80"
        );
        assert_eq!(normalize_uri("https://a.b/c d"), "https://a.b/c%20d");
        // 올바른 %XX는 그대로, 짝이 없는 %는 %25.
        assert_eq!(
            normalize_uri("https://a.b/%41%zz%"),
            "https://a.b/%41%zz%25"
        );
        assert_eq!(
            normalize_uri("a\"<>[\\]^`{|}b"),
            "a%22%3C%3E%5B%5C%5D%5E%60%7B%7C%7Db"
        );
        assert_eq!(
            normalize_uri("!#$&'()*+,-./:;=?@_~"),
            "!#$&'()*+,-./:;=?@_~"
        );
        assert_eq!(normalize_uri("😀"), "%F0%9F%98%80");
        assert_eq!(normalize_uri(""), "");
        assert_eq!(
            inline("[x](<https://a.b/c d>)"),
            vec![link("x", "https://a.b/c%20d")]
        );
        assert_eq!(inline("[x](<java script:alert(1)>)"), vec![link("x", "")]);
        assert_eq!(inline("[x](&#x6A;avascript:alert(1))"), vec![link("x", "")]);
        assert_eq!(inline("[x](a.b:80)"), vec![link("x", "")]);
        assert_eq!(inline("[x](//a.b:80/c)"), vec![link("x", "//a.b:80/c")]);
        assert_eq!(inline("[x](#frag)"), vec![link("x", "#frag")]);
    }

    #[test]
    fn 구두점_뒤에서_닫히는_굵게도_굵게로_보인다() {
        let bold = |text: &str| Span {
            text: text.into(),
            bold: true,
            ..Span::default()
        };
        // CommonMark는 `)` 뒤 `를` 앞의 `**`를 닫는 표시로 보지 않는다.
        assert_eq!(
            inline("**API(추정)**를 쓴다"),
            vec![bold("API(추정)"), plain("를 쓴다")]
        );
        assert_eq!(
            inline("결론은 **\"서명 필요\"**입니다. **끝.**이다"),
            vec![
                plain("결론은 "),
                bold("\"서명 필요\""),
                plain("입니다. "),
                bold("끝."),
                plain("이다")
            ]
        );
        // 짝이 없는 표시와 코드 안의 표시는 그대로 둔다.
        assert_eq!(inline("2 ** 3"), vec![plain("2 ** 3")]);
        assert_eq!(inline("열린 **표시만"), vec![plain("열린 **표시만")]);
    }

    #[test]
    fn 이어진_공백은_브라우저처럼_하나로_줄인다() {
        assert_eq!(inline("a   b  c"), vec![plain("a b c")]);
        assert_eq!(inline("Sentence.  Next"), vec![plain("Sentence. Next")]);
        assert_eq!(inline("a\tb"), vec![plain("a b")]);
        // 구간 경계를 넘어서도 하나만 남는다.
        assert_eq!(
            inline("a **b ** c"),
            vec![plain("a **b ** c")],
            "닫는 표시 앞이 공백이면 강조가 아니다"
        );
        assert_eq!(
            inline("a `  b  ` c"),
            vec![
                plain("a "),
                Span {
                    code: true,
                    ..plain("b ")
                },
                plain("c"),
            ]
        );
        assert_eq!(
            inline("`a   b`"),
            vec![Span {
                code: true,
                ..plain("a b")
            }]
        );
        // 줄바꿈 없는 공백은 줄이지 않는다.
        assert_eq!(inline("a&nbsp;&nbsp;b"), vec![plain("a\u{a0}\u{a0}b")]);
        // 강제 줄바꿈 뒤의 공백은 줄 머리라서 사라진다.
        assert_eq!(inline("x\\\n   y"), vec![plain("x\ny")]);
        assert_eq!(
            parse("<div>\n  <p>hi</p>\n</div>"),
            vec![Block::Text(vec![plain("<div> <p>hi</p> </div>")])]
        );
    }

    #[test]
    fn 각주는_번호_링크가_되고_본문_뒤에_목록으로_모인다() {
        let note = |text: &str, back: Vec<Span>| ListItem {
            task: None,
            blocks: vec![Block::Paragraph([vec![plain(text)], back].concat())],
        };
        assert_eq!(
            parse("a[^x] b[^y] c[^X]\n\n[^y]: second\n[^x]: first"),
            vec![
                Block::Paragraph(vec![
                    plain("a"),
                    link("1", "#user-content-fn-x"),
                    plain(" b"),
                    link("2", "#user-content-fn-y"),
                    plain(" c"),
                    link("1", "#user-content-fn-x"),
                ]),
                Block::Heading {
                    level: 2,
                    content: vec![plain("Footnotes")],
                },
                Block::List {
                    start: Some(1),
                    items: vec![
                        // 처음 참조된 순서대로, 참조마다 되돌아가는 링크가 붙는다.
                        note(
                            "first ",
                            vec![
                                link("↩", "#user-content-fnref-x"),
                                plain(" "),
                                link("↩2", "#user-content-fnref-x-2"),
                            ]
                        ),
                        note("second ", vec![link("↩", "#user-content-fnref-y")]),
                    ],
                },
            ]
        );
    }

    #[test]
    fn 정의가_없는_각주_표시는_글자로_남고_참조가_없는_정의는_보이지_않는다() {
        assert_eq!(parse("a[^1]"), vec![Block::Paragraph(vec![plain("a[^1]")])]);
        assert_eq!(parse("[^1]: unused"), vec![]);
        // 스트리밍 중: 정의가 도착하기 전까지는 글자다.
        assert_eq!(
            parse("a[^1]\n\n[^1"),
            vec![
                Block::Paragraph(vec![plain("a[^1]")]),
                Block::Paragraph(vec![plain("[^1")]),
            ]
        );
    }

    #[test]
    fn 문단으로_끝나지_않는_각주는_되돌아가는_링크가_따로_붙는다() {
        let blocks = parse("a[^1]\n\n[^1]:\n    ```\n    code\n    ```");
        let Some(Block::List { items, .. }) = blocks.get(2) else {
            panic!("각주 목록이 없다: {blocks:?}");
        };
        assert_eq!(
            items[0].blocks,
            vec![
                Block::Code("code\n".into()),
                Block::Text(vec![link("↩", "#user-content-fnref-1")]),
            ]
        );
    }

    #[test]
    fn 코드의_탭은_여덟_칸_단위로_펴고_마지막_줄바꿈은_그리지_않는다() {
        assert_eq!(pre_text("\ta\n"), "        a");
        assert_eq!(pre_text("ab\tc\n\td"), "ab      c\n        d");
        assert_eq!(pre_text("12345678\tx"), "12345678        x");
        assert_eq!(pre_text("한\t글"), "한       글");
        // 마지막 줄바꿈 하나만 빠진다.
        assert_eq!(pre_text("a\n\n"), "a\n");
        assert_eq!(pre_text(""), "");
    }

    #[test]
    fn 자동_링크_검사는_어떤_조각에서도_패닉하지_않는다() {
        let sample =
            "(www.a_b.c/d(e))&amp;] https://가.b/c?d=e&f;g <a@b.co.kr>. x@y.z- WWW.q.r:8/s";
        for start in 0..sample.len() {
            for end in start..=sample.len() {
                if let Some(piece) = sample.get(start..end) {
                    let _ = find_autolink(piece, None);
                    let _ = normalize_uri(piece);
                }
            }
        }
    }
}

#[cfg(test)]
mod selection_tests {
    use super::{CopySelection, ElementId, FONT_PX, LINE_HEIGHT, markdown};
    use gpui::prelude::*;
    use gpui::{
        Context, Modifiers, MouseButton, Point, Render, TestAppContext, VisualTestContext, Window,
        div, px,
    };
    use rever_ui_kit::theme::{Theme, ThemeMode};

    /// A window holding one selectable message `width` pixels wide: narrow
    /// enough and its text wraps.
    struct Host {
        text: &'static str,
        width: f32,
    }

    impl Render for Host {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::resolve(ThemeMode::Light, window.appearance());
            div()
                .id("md-host")
                .debug_selector(|| "md-host".into())
                .p(px(20.0))
                .w(px(40.0 + self.width))
                .child(markdown(self.text, &theme).id("md-sel").selectable(true))
        }
    }

    fn open<'a>(
        cx: &'a mut TestAppContext,
        text: &'static str,
        width: f32,
    ) -> (u64, &'a mut VisualTestContext) {
        // The app binds this at startup before any window opens.
        cx.update(|cx: &mut gpui::App| {
            cx.bind_keys([gpui::KeyBinding::new(
                "cmd-c",
                CopySelection,
                Some("MarkdownSelection"),
            )]);
        });
        let (_view, cx) = cx
            .add_window_view(|_window: &mut Window, _cx: &mut Context<Host>| Host { text, width });
        cx.run_until_parked();
        (crate::selection::key(&ElementId::from("md-sel")), cx)
    }

    fn drag(cx: &mut VisualTestContext, start: Point<gpui::Pixels>, end: Point<gpui::Pixels>) {
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
    }

    /// A point just inside the glyph that starts at caret `at` on its line.
    fn on(carets: &[Point<gpui::Pixels>], at: usize) -> Point<gpui::Pixels> {
        carets[at] + gpui::point(px(1.0), px(FONT_PX * LINE_HEIGHT / 2.0))
    }

    #[gpui::test]
    fn 드래그하면_텍스트가_선택된다(cx: &mut TestAppContext) {
        let (owner, cx) = open(
            cx,
            "가나다라마바사 아자차카타파하\n\n둘째 문단의 글자들",
            600.0,
        );
        let carets = crate::selection::carets(owner);
        drag(
            cx,
            on(&carets, 0),
            carets[0] + gpui::point(px(120.0), px(12.0)),
        );
        let selected = crate::selection::selected_text(owner);
        assert!(selected.is_some(), "nothing was selected");
        let selected = selected.unwrap();
        assert!(
            "가나다라마바사 아자차카타파하".starts_with(&selected)
                && !selected.is_empty()
                && !selected.contains('문'),
            "selection picked text from the first paragraph: {selected}"
        );
        // One visual line of selection is one rectangle, not one per glyph.
        let rects = crate::selection::highlight(owner);
        assert_eq!(rects.len(), 1, "a single-line selection is one rect");
        // Cmd-C copies it: the drag moved focus to the markdown so the
        // contextual binding reaches the element's action.
        cx.simulate_keystrokes("cmd-c");
        let clipboard = cx
            .read_from_clipboard()
            .and_then(|item| item.text().to_owned());
        assert_eq!(
            clipboard,
            Some(selected.clone()),
            "cmd-c copied the selection"
        );
        assert!(
            crate::selection::selected_text(owner).is_none(),
            "copying clears the selection"
        );
    }

    #[gpui::test]
    fn 줄이_바뀌어도_다음_줄_첫_글자부터_칠해진다(cx: &mut TestAppContext) {
        let text: &'static str = concat!(
            "가나다라마 바사아자차 카타파하가 나다라마바 ",
            "가나다라마 바사아자차 카타파하가 나다라마바 ",
            "가나다라마 바사아자차 카타파하가 나다라마바"
        );
        let (owner, cx) = open(cx, text, 120.0);
        let carets = crate::selection::carets(owner);
        // Carets are char boundaries: this is the byte offset of each.
        let bytes: Vec<usize> = text
            .char_indices()
            .map(|(at, _)| at)
            .chain([text.len()])
            .collect();
        assert_eq!(carets.len(), bytes.len(), "one caret per char boundary");
        // A caret at a wrap boundary sits at the end of the line above. So the
        // first caret on a lower line is the boundary after that line's first
        // glyph and the next one starts its third glyph.
        let second = (1..carets.len())
            .find(|&at| carets[at].y != carets[0].y)
            .expect("the text wraps");
        assert_eq!(carets[second - 1].y, carets[0].y);
        let on_second = (second..carets.len())
            .take_while(|&at| carets[at].y == carets[second].y)
            .count();
        assert!(on_second >= 3, "the second line holds enough glyphs");
        let third_glyph = second + 1;
        drag(cx, on(&carets, 0), on(&carets, third_glyph));
        let rects = crate::selection::highlight(owner);
        assert_eq!(rects.len(), 2, "one rect per visual line: {rects:?}");
        assert_eq!(rects[0].origin.y, carets[0].y);
        assert_eq!(rects[1].origin.y, carets[second].y);
        assert_eq!(
            rects[1].origin.x, carets[0].x,
            "the second line is painted from its first glyph: {rects:?}"
        );
        assert_eq!(rects[1].right(), carets[third_glyph].x);
        assert!(
            rects[0].right() >= carets[second - 2].x,
            "the last glyph of the first line is painted too: {rects:?}"
        );
        assert_eq!(
            crate::selection::selected_text(owner).as_deref(),
            Some(&text[..bytes[third_glyph]])
        );
    }

    #[gpui::test]
    fn 커서가_글자_중간을_지나면_그_글자도_선택된다(cx: &mut TestAppContext) {
        let text = "가나다라마바사";
        let (owner, cx) = open(cx, text, 600.0);
        let carets = crate::selection::carets(owner);
        // Release a little past the middle of the fourth glyph.
        let past_middle = px(f32::from(carets[4].x - carets[3].x) / 2.0 + 1.0);
        let end = carets[3] + gpui::point(past_middle, px(FONT_PX * LINE_HEIGHT / 2.0));
        drag(cx, on(&carets, 0), end);
        assert_eq!(
            crate::selection::selected_text(owner).as_deref(),
            Some("가나다라")
        );
    }

    #[gpui::test]
    fn 왼쪽으로_드래그해도_누른_글자는_선택에_남는다(cx: &mut TestAppContext) {
        let text = "가나다라마바사";
        let (owner, cx) = open(cx, text, 600.0);
        let carets = crate::selection::carets(owner);
        // Press on the fifth glyph and drag back to the second.
        drag(cx, on(&carets, 4), on(&carets, 1));
        assert_eq!(
            crate::selection::selected_text(owner).as_deref(),
            Some("나다라마")
        );
    }
}
