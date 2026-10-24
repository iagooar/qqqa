use nu_ansi_term::{Color, Style};
use std::io::Write as _;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

static COLOR_OUTPUT_ENABLED: AtomicBool = AtomicBool::new(true);

/// Write to stdout and flush. When stdout is a closed pipe (`qq ... | head`),
/// end the process quietly with status 0, as other filters do, instead of the
/// panic that `print!` gives.
pub fn write_stdout(text: &str) {
    let mut out = std::io::stdout().lock();
    if let Err(err) = out.write_all(text.as_bytes()).and_then(|()| out.flush())
        && err.kind() == std::io::ErrorKind::BrokenPipe
    {
        std::process::exit(0);
    }
}

/// `print!` that ends quietly on a closed pipe; see [`write_stdout`].
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => { $crate::formatting::write_stdout(&format!($($arg)*)) };
}

/// `println!` that ends quietly on a closed pipe; see [`write_stdout`].
#[macro_export]
macro_rules! outln {
    () => { $crate::formatting::write_stdout("\n") };
    ($($arg:tt)*) => { $crate::formatting::write_stdout(&(format!($($arg)*) + "\n")) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkupTag {
    Bold,
    Cmd,
    Info,
    File,
    Warn,
    Code,
}

/// Toggle ANSI coloring at runtime. Call once per run from the CLI after
/// detecting whether stdout supports color output.
pub fn set_color_output_enabled(enabled: bool) {
    COLOR_OUTPUT_ENABLED.store(enabled, Ordering::Relaxed);
}

fn color_output_enabled() -> bool {
    COLOR_OUTPUT_ENABLED.load(Ordering::Relaxed)
}

fn style_for_stack(stack: &[MarkupTag]) -> Style {
    let mut st = Style::new();
    for t in stack {
        match t {
            MarkupTag::Bold => st = st.bold(),
            MarkupTag::Cmd => st = st.fg(Color::Green),
            MarkupTag::Info => st = st.fg(Color::Cyan),
            MarkupTag::File => st = st.fg(Color::Magenta),
            MarkupTag::Warn => st = st.fg(Color::Yellow),
            MarkupTag::Code => st = st.fg(Color::Cyan),
        }
    }
    st
}

fn unescape(input: &str) -> String {
    input
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn tag_from_name(name: &str) -> Option<MarkupTag> {
    match name {
        "bold" | "strong" | "b" => Some(MarkupTag::Bold),
        "cmd" => Some(MarkupTag::Cmd),
        "info" => Some(MarkupTag::Info),
        "file" => Some(MarkupTag::File),
        "warn" => Some(MarkupTag::Warn),
        "code" => Some(MarkupTag::Code),
        _ => None,
    }
}

fn push_with_style(out: &mut String, text: &str, stack: &[MarkupTag]) {
    if text.is_empty() {
        return;
    }
    let text = unescape(text);
    if text.is_empty() {
        return;
    }
    if stack.is_empty() || !color_output_enabled() {
        out.push_str(&text);
    } else {
        out.push_str(&style_for_stack(stack).paint(text).to_string());
    }
}

fn split_emit_tail(text: &str) -> (&str, usize) {
    if let Some(pos) = text.rfind('&') {
        let tail = &text[pos..];
        if !tail.contains(';') && tail.len() < 5 {
            return (&text[..pos], text.len() - pos);
        }
    }
    (text, 0)
}

#[derive(Default)]
struct XmlishStreamingParser {
    stack: Vec<MarkupTag>,
    pending: String,
}

impl XmlishStreamingParser {
    fn new() -> Self {
        Self {
            stack: Vec::new(),
            pending: String::new(),
        }
    }

    fn process(&mut self, chunk: &str, out: &mut String) {
        if chunk.is_empty() {
            return;
        }
        self.pending.push_str(chunk);
        self.drain(out, false);
    }

    fn finish(&mut self, out: &mut String) {
        self.drain(out, true);
    }

    /// Emit everything that can no longer change. Only known tags are consumed;
    /// any other `<` is ordinary text (shell redirections, `<PID>` placeholders).
    /// Unless `at_end`, a possible tag or entity at the end stays pending.
    fn drain(&mut self, out: &mut String, at_end: bool) {
        let mut processed = 0usize;
        while processed < self.pending.len() {
            let Some(rel) = self.pending[processed..].find('<') else {
                let tail = &self.pending[processed..];
                let (emit, tail_len) = if at_end {
                    (tail, 0)
                } else {
                    split_emit_tail(tail)
                };
                push_with_style(out, emit, &self.stack);
                processed = self.pending.len() - tail_len;
                break;
            };
            let start = processed + rel;
            push_with_style(out, &self.pending[processed..start], &self.stack);
            match parse_tag(&self.pending[start..]) {
                TagParse::Complete { len, action } => {
                    self.apply(action, out);
                    processed = start + len;
                }
                TagParse::Partial if !at_end => {
                    processed = start;
                    break;
                }
                TagParse::Partial | TagParse::NotTag => {
                    push_with_style(out, "<", &self.stack);
                    processed = start + 1;
                }
            }
        }
        self.pending.drain(..processed);
    }

    fn apply(&mut self, action: TagAction, out: &mut String) {
        match action {
            TagAction::LineBreak => out.push('\n'),
            TagAction::Open(tag) => self.stack.push(tag),
            TagAction::Close(tag) => {
                if let Some(pos) = self.stack.iter().rposition(|t| *t == tag) {
                    self.stack.remove(pos);
                }
            }
            TagAction::Ignore => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagAction {
    LineBreak,
    Open(MarkupTag),
    Close(MarkupTag),
    Ignore,
}

#[derive(Debug, PartialEq, Eq)]
enum TagParse {
    /// A known tag of `len` bytes starts the input.
    Complete { len: usize, action: TagAction },
    /// The input ends inside something that can still become a known tag.
    Partial,
    /// The `<` at the start is ordinary text.
    NotTag,
}

const TAG_NAMES: &[&str] = &[
    "bold", "strong", "b", "cmd", "info", "file", "warn", "code", "br",
];

/// Parse `<` ws* `/`? ws* name ws* `/`? ws* `>` where name is a known tag name
/// (ASCII case-insensitive). The input starts with `<`.
fn parse_tag(input: &str) -> TagParse {
    let bytes = input.as_bytes();
    let skip_ws = |mut i: usize| {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    let mut i = skip_ws(1);
    let closing = i < bytes.len() && bytes[i] == b'/';
    if closing {
        i = skip_ws(i + 1);
    }
    let name_start = i;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    let name = input[name_start..i].to_ascii_lowercase();
    if i == bytes.len() {
        let could_grow = TAG_NAMES.iter().any(|t| t.starts_with(name.as_str()));
        return if could_grow {
            TagParse::Partial
        } else {
            TagParse::NotTag
        };
    }
    if !TAG_NAMES.contains(&name.as_str()) {
        return TagParse::NotTag;
    }
    i = skip_ws(i);
    let self_closing = i < bytes.len() && bytes[i] == b'/';
    if self_closing {
        i = skip_ws(i + 1);
    }
    if i == bytes.len() {
        return TagParse::Partial;
    }
    if bytes[i] != b'>' {
        return TagParse::NotTag;
    }
    let action = if name == "br" {
        if closing {
            TagAction::Ignore
        } else {
            TagAction::LineBreak
        }
    } else {
        match (tag_from_name(&name), closing, self_closing) {
            (Some(tag), true, _) => TagAction::Close(tag),
            (Some(tag), false, false) => TagAction::Open(tag),
            _ => TagAction::Ignore,
        }
    };
    TagParse::Complete { len: i + 1, action }
}

/// Render our pseudo-XML to ANSI-colored text.
/// Supported tags: <bold>, <cmd>, <info>, <file>, <warn>, <code>, <br/>
pub fn render_xmlish_to_ansi(input: &str) -> String {
    let mut parser = XmlishStreamingParser::new();
    let mut out = String::new();
    parser.process(input, &mut out);
    parser.finish(&mut out);
    out
}

/// Incremental formatter that mirrors `render_xmlish_to_ansi` but streams output.
pub struct StreamingFormatter {
    parser: XmlishStreamingParser,
    raw: String,
    rendered: String,
}

impl Default for StreamingFormatter {
    fn default() -> Self {
        Self {
            parser: XmlishStreamingParser::new(),
            raw: String::new(),
            rendered: String::new(),
        }
    }
}

impl StreamingFormatter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a chunk of model output (a delta, not the text so far); returns the
    /// formatted delta (if any) to print immediately.
    pub fn push(&mut self, chunk: &str) -> Option<String> {
        if chunk.is_empty() {
            return None;
        }
        self.raw.push_str(chunk);
        self.ingest_suffix(chunk)
    }

    /// Flush any buffered text that was waiting for tag/entity completion.
    pub fn flush(&mut self) -> Option<String> {
        let mut tail = String::new();
        self.parser.finish(&mut tail);
        if tail.is_empty() {
            None
        } else {
            self.rendered.push_str(&tail);
            Some(tail)
        }
    }

    /// Retrieve the full formatted text seen so far.
    pub fn rendered(&self) -> String {
        render_xmlish_to_ansi(&self.raw)
    }

    fn ingest_suffix(&mut self, suffix: &str) -> Option<String> {
        if suffix.is_empty() {
            return None;
        }
        let mut delta = String::new();
        self.parser.process(suffix, &mut delta);
        if delta.is_empty() {
            None
        } else {
            self.rendered.push_str(&delta);
            Some(delta)
        }
    }
}

/// Print a transient status to stderr (no spinner to keep deps small).
pub fn status_thinking() {
    if color_output_enabled() {
        eprintln!("{}", Color::Yellow.paint("Thinking…"));
    } else {
        eprintln!("Thinking…");
    }
}

/// Print streamed token to stdout. We avoid buffering to keep latency low.
pub fn print_stream_token(token: &str) {
    write_stdout(token);
}

/// Print a full, non-streamed assistant message.
pub fn print_assistant_text(text: &str, raw: bool) {
    if raw {
        crate::outln!("{}", text);
    } else {
        let rendered = render_xmlish_to_ansi(text);
        let compacted = compact_blank_lines(&rendered);
        crate::outln!("{}", compacted.trim_end());
    }
}

/// Handle to a simple loading animation (cyclic dots) printed to stderr.
pub struct LoadingAnimation {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl LoadingAnimation {
    pub fn stop(self) {
        drop(self);
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            // Wake the thread from its frame wait so stopping is immediate.
            h.thread().unpark();
            let _ = h.join();
            eprint!("\r        \r");
            let _ = std::io::stderr().flush();
        }
    }
}

impl Drop for LoadingAnimation {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Start the cyclic dot animation on stderr. Returns a handle that stops it when dropped.
/// Nothing is drawn when stderr is not a terminal.
pub fn start_loading_animation() -> LoadingAnimation {
    let stop = Arc::new(AtomicBool::new(false));
    if !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return LoadingAnimation { stop, handle: None };
    }
    let stop_cl = stop.clone();
    let handle = thread::spawn(move || {
        let mut n: u8 = 1;
        while !stop_cl.load(Ordering::SeqCst) {
            let dots = match n {
                1 => ".",
                2 => "..",
                _ => "...",
            };
            eprint!("\r{}", dots);
            let _ = std::io::stderr().flush();
            n = if n >= 3 { 1 } else { n + 1 };
            thread::park_timeout(Duration::from_millis(300));
        }
    });
    LoadingAnimation {
        stop,
        handle: Some(handle),
    }
}

/// Reduce runs of blank lines to a single blank line and normalize newlines.
pub fn compact_blank_lines(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_blank = false;
    for line in input.replace('\r', "").split('\n') {
        let is_blank = line.trim().is_empty();
        if is_blank {
            if !last_blank {
                out.push('\n');
                last_blank = true;
            }
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(line);
            out.push('\n');
            last_blank = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        COLOR_OUTPUT_ENABLED, StreamingFormatter, render_xmlish_to_ansi, set_color_output_enabled,
    };
    use std::sync::atomic::Ordering;
    use std::sync::{Mutex, OnceLock};

    /// Feed `chunks` to a StreamingFormatter and return exactly what it printed.
    fn printed(chunks: &[&str]) -> String {
        let mut fmt = StreamingFormatter::new();
        let mut out = String::new();
        for chunk in chunks {
            if let Some(delta) = fmt.push(chunk) {
                out.push_str(&delta);
            }
        }
        if let Some(tail) = fmt.flush() {
            out.push_str(&tail);
        }
        out
    }

    fn printed_in_chunks_of(input: &str, size: usize) -> String {
        let chars: Vec<char> = input.chars().collect();
        let pieces: Vec<String> = chars
            .chunks(size.max(1))
            .map(|c| c.iter().collect())
            .collect();
        let refs: Vec<&str> = pieces.iter().map(String::as_str).collect();
        printed(&refs)
    }

    /// Every string over `alphabet` with length 0..=max_len.
    fn all_strings(alphabet: &[char], max_len: usize) -> Vec<String> {
        let mut all = vec![String::new()];
        let mut layer = vec![String::new()];
        for _ in 0..max_len {
            let mut next = Vec::new();
            for s in &layer {
                for c in alphabet {
                    let mut t = s.clone();
                    t.push(*c);
                    next.push(t);
                }
            }
            all.extend(next.iter().cloned());
            layer = next;
        }
        all
    }

    #[test]
    fn printed_stream_matches_full_render_for_markup() {
        with_color_setting(false, || {
            for input in [
                "<cmd>ls -la</cmd> then <info>done</info>",
                "Fish &amp; chips <bold>rule</bold>.",
                "<cmd>echo hi</cmd><warn>careful</warn>",
                "a<br/>b<br />c<BR>d",
            ] {
                let full = render_xmlish_to_ansi(input);
                for size in 1..=input.len() {
                    assert_eq!(
                        printed_in_chunks_of(input, size),
                        full,
                        "{input:?} in chunks of {size}"
                    );
                }
            }
        });
    }

    #[test]
    fn printed_stream_matches_full_render_with_color() {
        with_color_setting(true, || {
            let input = "<cmd>ls</cmd> <info>x</info>";
            assert_eq!(printed(&[input]), render_xmlish_to_ansi(input));
        });
    }

    #[test]
    fn text_that_is_not_a_known_tag_is_kept() {
        with_color_setting(false, || {
            let cases = [
                (
                    "<cmd>sort < in.txt > out.txt</cmd>",
                    "sort < in.txt > out.txt",
                ),
                ("<cmd>cat <<EOF > f.txt</cmd>", "cat <<EOF > f.txt"),
                (
                    "<cmd>diff <(ls a) <(ls b)</cmd> then <info>done</info>",
                    "diff <(ls a) <(ls b) then done",
                ),
                ("<cmd>kill -9 <PID></cmd>", "kill -9 <PID>"),
                ("Use x < 5 and y > 3 here", "Use x < 5 and y > 3 here"),
                ("<cmd>echo 1 &lt; 2</cmd>", "echo 1 < 2"),
            ];
            for (input, want) in cases {
                assert_eq!(render_xmlish_to_ansi(input), want, "{input:?}");
                assert_eq!(printed_in_chunks_of(input, 1), want, "{input:?} streamed");
            }
        });
    }

    #[test]
    fn closing_tag_after_unknown_angle_text_still_closes() {
        with_color_setting(true, || {
            let out = render_xmlish_to_ansi("<cmd>diff <(ls a)</cmd> plain");
            assert!(out.ends_with(" plain"), "style must not leak: {out:?}");
        });
    }

    #[test]
    fn stray_angle_bracket_does_not_stall_the_stream() {
        with_color_setting(false, || {
            let mut fmt = StreamingFormatter::new();
            let first = fmt.push("if a < b then").unwrap_or_default();
            assert_eq!(first, "if a < b then");
        });
    }

    #[test]
    fn repeated_tokens_are_not_dropped() {
        with_color_setting(false, || {
            assert_eq!(printed(&["-", "-", "x"]), "--x");
            assert_eq!(printed(&["1", "1", " files"]), "11 files");
            assert_eq!(printed(&["-", "-force"]), "--force");
        });
    }

    /// For every short string without tag names, rendering and streaming
    /// (split at any point) return the input unchanged.
    #[test]
    fn exhaustive_plain_text_round_trips() {
        with_color_setting(false, || {
            for s in all_strings(&['<', '>', '/', ' ', 'a', 'x', '5'], 5) {
                assert_eq!(render_xmlish_to_ansi(&s), s, "render {s:?}");
                let chars: Vec<char> = s.chars().collect();
                for cut in 0..=chars.len() {
                    let a: String = chars[..cut].iter().collect();
                    let b: String = chars[cut..].iter().collect();
                    assert_eq!(printed(&[&a, &b]), s, "split {a:?} | {b:?}");
                }
            }
        });
    }

    /// For every short string that may contain tags, streaming split at any
    /// point prints the same text as one full render.
    #[test]
    fn exhaustive_streaming_matches_full_render() {
        with_color_setting(false, || {
            for s in all_strings(&['<', '>', '/', 'b', 'r', 'x', '&'], 5) {
                let full = render_xmlish_to_ansi(&s);
                let chars: Vec<char> = s.chars().collect();
                for cut in 0..=chars.len() {
                    let a: String = chars[..cut].iter().collect();
                    let b: String = chars[cut..].iter().collect();
                    assert_eq!(printed(&[&a, &b]), full, "split {a:?} | {b:?}");
                }
                assert_eq!(printed_in_chunks_of(&s, 1), full, "chars of {s:?}");
            }
        });
    }

    #[test]
    fn render_xmlish_emits_color_when_enabled() {
        let rendered = with_color_setting(true, || render_xmlish_to_ansi("<cmd>ls</cmd>"));
        assert!(
            rendered.contains("\u{1b}[32m"),
            "expected ANSI codes when color enabled"
        );
    }

    #[test]
    fn render_xmlish_strips_color_when_disabled() {
        let rendered = with_color_setting(false, || render_xmlish_to_ansi("<cmd>ls</cmd>"));
        assert_eq!(rendered, "ls");
    }

    fn with_color_setting<F, R>(enabled: bool, func: F) -> R
    where
        F: FnOnce() -> R,
    {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = COLOR_OUTPUT_ENABLED.load(Ordering::Relaxed);
        set_color_output_enabled(enabled);
        let result = func();
        set_color_output_enabled(prev);
        drop(guard);
        result
    }
}
