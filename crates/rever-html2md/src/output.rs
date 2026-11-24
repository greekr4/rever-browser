use std::borrow::Cow;

/// Finished Markdown fragments. Joining outputs moves their roots, not their
/// text or descendant vectors. Both flattening and destruction are iterative.
#[derive(Default)]
pub(crate) struct Output {
    parts: Vec<Part>,
    len: usize,
    last: Option<char>,
    trailing_newlines: usize,
}

enum Part {
    Text(Cow<'static, str>),
    Group(Output),
}

impl Output {
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn last_char(&self) -> Option<char> {
        self.last
    }

    pub(crate) fn trailing_newlines(&self) -> usize {
        self.trailing_newlines
    }

    fn extend_tail(&mut self, len: usize, last: char, trailing_newlines: usize) {
        self.len += len;
        self.last = Some(last);
        self.trailing_newlines = if trailing_newlines == len {
            self.trailing_newlines + trailing_newlines
        } else {
            trailing_newlines
        };
    }

    pub(crate) fn push_text(&mut self, text: Cow<'static, str>) {
        if let Some(last) = text.chars().next_back() {
            self.extend_tail(
                text.len(),
                last,
                text.len() - text.trim_end_matches('\n').len(),
            );
            self.parts.push(Part::Text(text));
        }
    }

    pub(crate) fn append(&mut self, output: Self) {
        if let Some(last) = output.last {
            self.extend_tail(output.len, last, output.trailing_newlines);
            self.parts.push(Part::Group(output));
        }
    }

    pub(crate) fn into_string(mut self) -> String {
        // Ordinary inline output already has a contiguous buffer. Keep that
        // allocation instead of adding a copy to the common, non-table path.
        if self.parts.len() == 1 {
            match self.parts.pop().expect("one output fragment") {
                Part::Text(Cow::Owned(text)) => return text,
                part => self.parts.push(part),
            }
        }
        let mut text = String::with_capacity(self.len);
        let mut pending = std::mem::take(&mut self.parts);
        pending.reverse();
        while let Some(part) = pending.pop() {
            match part {
                Part::Text(fragment) => {
                    #[cfg(test)]
                    record_copy(fragment.len());
                    text.push_str(&fragment);
                }
                Part::Group(mut output) => pending.extend(output.parts.drain(..).rev()),
            }
        }
        text
    }
}

impl From<String> for Output {
    fn from(text: String) -> Self {
        let mut output = Self::default();
        output.push_text(text.into());
        output
    }
}

impl From<&'static str> for Output {
    fn from(text: &'static str) -> Self {
        let mut output = Self::default();
        output.push_text(text.into());
        output
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        // Dropping nested Vec<Output> normally recurses. Empty each group's
        // children before dropping it, including outputs never flattened.
        let mut pending = std::mem::take(&mut self.parts);
        while let Some(part) = pending.pop() {
            if let Part::Group(mut output) = part {
                pending.append(&mut output.parts);
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    static COPIED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_copy(bytes: usize) {
    COPIED_BYTES.with(|count| count.set(count.get() + bytes));
}

#[cfg(test)]
pub(crate) fn measure_copies<T>(convert: impl FnOnce() -> T) -> (T, usize) {
    COPIED_BYTES.with(|count| count.set(0));
    let output = convert();
    (output, COPIED_BYTES.with(std::cell::Cell::get))
}
