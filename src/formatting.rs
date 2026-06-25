use nu_ansi_term::{Color, Style};
use std::io::Write as _;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

static COLOR_OUTPUT_ENABLED: AtomicBool = AtomicBool::new(true);

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
        let mut processed = 0usize;
        while processed < self.pending.len() {
            let remaining = &self.pending[processed..];
            if remaining.is_empty() {
                break;
            }
            if let Some(rel) = remaining.find('<') {
                let start = processed + rel;
                if start > processed {
                    let text = &self.pending[processed..start];
                    push_with_style(out, text, &self.stack);
                }
                let after = &self.pending[start..];
                if let Some(end_rel) = after.find('>') {
                    let end = start + end_rel;
                    let raw = self.pending[start + 1..end].trim().to_string();
                    self.handle_tag(&raw, out);
                    processed = end + 1;
                } else {
                    // Incomplete tag; keep from start onward for next chunk
                    processed = start;
                    break;
                }
            } else {
                // No more tags, emit safe portion
                let tail = &self.pending[processed..];
                let (emit, tail_len) = split_emit_tail(tail);
                push_with_style(out, emit, &self.stack);
                processed = self.pending.len() - tail_len;
                break;
            }
        }
        self.pending.drain(..processed);
    }

    fn finish(&mut self, out: &mut String) {
        if !self.pending.is_empty() {
            push_with_style(out, &self.pending, &self.stack);
            self.pending.clear();
        }
    }

    fn handle_tag(&mut self, raw: &str, out: &mut String) {
        if raw.is_empty() {
            return;
        }
        let raw_trim = raw.trim();
        let normalized = raw_trim.replace(' ', "");
        let lowered = normalized.trim_end_matches('/').to_ascii_lowercase();
        if lowered == "br" {
            out.push('\n');
            return;
        }
        if raw_trim.starts_with('/') {
            let name = raw_trim[1..].trim().to_ascii_lowercase();
            if let Some(tag) = tag_from_name(&name) {
                if let Some(pos) = self.stack.iter().rposition(|t| *t == tag) {
                    self.stack.remove(pos);
                }
            }
            return;
        }
        if let Some(tag) = tag_from_name(&lowered) {
            self.stack.push(tag);
        }
    }
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

    /// Push a chunk of model output; returns the formatted delta (if any) to print immediately.
    pub fn push(&mut self, chunk: &str) -> Option<String> {
        if chunk.is_empty() {
            return None;
        }
        if !self.raw.is_empty()
            && chunk.len() >= self.raw.len()
            && chunk.starts_with(self.raw.as_str())
        {
            let suffix = &chunk[self.raw.len()..];
            self.raw.clear();
            self.raw.push_str(chunk);
            return self.ingest_suffix(suffix);
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
    print!("{}", token);
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// Print a full, non-streamed assistant message.
pub fn print_assistant_text(text: &str, raw: bool) {
    if raw {
        println!("{}", text);
    } else {
        let rendered = render_xmlish_to_ansi(text);
        let compacted = compact_blank_lines(&rendered);
        println!("{}", compacted.trim_end());
    }
}

/// Handle to a simple loading animation (cyclic dots) printed to stderr.
pub struct LoadingAnimation {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl LoadingAnimation {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        // Clear the line
        eprint!("\r        \r");
        let _ = std::io::stderr().flush();
    }
}

impl Drop for LoadingAnimation {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        eprint!("\r        \r");
        let _ = std::io::stderr().flush();
    }
}

/// Start the cyclic dot animation on stderr. Returns a handle that stops it when dropped.
pub fn start_loading_animation() -> LoadingAnimation {
    let stop = Arc::new(AtomicBool::new(false));
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
            thread::sleep(Duration::from_millis(300));
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

    fn stream_chunks(input: &str, chunk_size: usize) -> String {
        let mut fmt = StreamingFormatter::new();
        for chunk in input.as_bytes().chunks(chunk_size.max(1)) {
            let piece = std::str::from_utf8(chunk).unwrap();
            let _ = fmt.push(piece);
        }
        let _ = fmt.flush();
        fmt.rendered()
    }

    #[test]
    fn streaming_matches_full_render_for_basic_markup() {
        let input = "<cmd>ls -la</cmd> then <info>done</info>";
        let streamed = stream_chunks(input, 3);
        let rendered = render_xmlish_to_ansi(input);
        assert_eq!(streamed, rendered);
    }

    #[test]
    fn streaming_handles_entities_split_across_chunks() {
        let input = "Fish &amp; chips <bold>rule</bold>.";
        let streamed = stream_chunks(input, 2);
        let rendered = render_xmlish_to_ansi(input);
        assert_eq!(streamed, rendered);
    }

    #[test]
    fn streaming_handles_incomplete_tags() {
        let input = "<cmd>echo hi</cmd><warn>careful</warn>";
        let streamed = stream_chunks(input, 1);
        let rendered = render_xmlish_to_ansi(input);
        assert_eq!(streamed, rendered);
    }

    #[test]
    fn streaming_handles_cumulative_chunks() {
        with_color_setting(true, || {
            let chunks = [
                "<cmd>ffmpeg -i input.mov output.mp4</cmd>",
                "<cmd>ffmpeg -i input.mov output.mp4</cmd> <info>done</info>",
                "<cmd>ffmpeg -i input.mov output.mp4</cmd> <info>done</info>",
            ];
            let mut fmt = StreamingFormatter::new();
            let mut printed = String::new();
            for chunk in chunks {
                if let Some(delta) = fmt.push(chunk) {
                    printed.push_str(&delta);
                }
            }
            if let Some(tail) = fmt.flush() {
                printed.push_str(&tail);
            }
            let final_render = fmt.rendered();
            let expected =
                render_xmlish_to_ansi("<cmd>ffmpeg -i input.mov output.mp4</cmd> <info>done</info>");
            assert_eq!(final_render, expected);
            assert_eq!(printed, expected);
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
        let guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let prev = COLOR_OUTPUT_ENABLED.load(Ordering::Relaxed);
        set_color_output_enabled(enabled);
        let result = func();
        set_color_output_enabled(prev);
        drop(guard);
        result
    }
}
