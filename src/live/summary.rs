use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use markdown_to_ansi::{Options as MarkdownOptions, render};

use crate::{
    ai::{LanguageModelProvider, SummaryRequest, codex},
    live::whisper::TranscriptSink,
};

#[derive(Debug, Clone)]
pub struct TranscriptOptions {
    pub summary_path: Option<PathBuf>,
    pub raw_transcript_path: Option<PathBuf>,
    pub instruction: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub render_terminal: bool,
}

pub struct TranscriptSinkBuilder {
    options: TranscriptOptions,
}

pub struct RawTranscriptSink {
    store: SummaryStore,
    terminal: TerminalRenderer,
}

pub struct LiveSummarizer {
    provider: Box<dyn LanguageModelProvider>,
    segmenter: TranscriptSegmenter,
    store: SummaryStore,
    state: SummaryState,
    terminal: TerminalRenderer,
    instruction: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
}

#[derive(Debug, Default)]
struct SummaryState {
    markdown: String,
}

#[derive(Debug)]
struct SummaryStore {
    summary_path: Option<PathBuf>,
    raw_transcript_path: Option<PathBuf>,
}

struct TerminalRenderer {
    render_terminal: bool,
    markdown_options: MarkdownOptions,
}

#[derive(Debug, Clone)]
pub struct SegmenterConfig {
    pub target_interval: Duration,
    pub force_interval: Duration,
    pub min_chars: usize,
    pub force_min_chars: usize,
    pub max_chars: usize,
    pub carryover_chars: usize,
}

#[derive(Debug)]
struct TranscriptSegmenter {
    config: SegmenterConfig,
    pending: String,
    segment_started_at: Option<Instant>,
    next_segment_id: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct SummarySegment {
    id: u64,
    raw_text: String,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            target_interval: Duration::from_millis(52_500),
            force_interval: Duration::from_secs(105),
            min_chars: 525,
            force_min_chars: 140,
            max_chars: 1575,
            carryover_chars: 160,
        }
    }
}

impl LiveSummarizer {
    pub fn new(options: TranscriptOptions) -> Result<Self> {
        Self::with_provider(options, Box::new(codex::CodexProvider))
    }

    fn with_provider(
        options: TranscriptOptions,
        provider: Box<dyn LanguageModelProvider>,
    ) -> Result<Self> {
        let Some(summary_path) = options.summary_path else {
            bail!("summary path is required for live summaries");
        };

        provider.ensure_authenticated()?;

        let store = SummaryStore::new(Some(summary_path), options.raw_transcript_path)?;
        let markdown = store.load_summary()?;

        Ok(Self {
            provider,
            segmenter: TranscriptSegmenter::new(SegmenterConfig::default()),
            store,
            state: SummaryState { markdown },
            terminal: TerminalRenderer::new(options.render_terminal),
            instruction: options.instruction,
            model: options.model,
            reasoning: options.reasoning,
        })
    }

    pub fn accept_raw_text(&mut self, text: &str) -> Result<()> {
        self.terminal.render_raw(text)?;
        self.store.append_raw(text)?;

        if let Some(segment) = self.segmenter.push(text, Instant::now()) {
            self.summarize_segment(segment)?;
        }

        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        if let Some(segment) = self.segmenter.flush() {
            self.summarize_segment(segment)?;
        }
        Ok(())
    }

    fn summarize_segment(&mut self, segment: SummarySegment) -> Result<()> {
        let instruction = self.summary_instruction();
        let transcript = format_transcript_text(&segment.raw_text);
        let summary = self.provider.summarize(SummaryRequest {
            transcript: &transcript,
            prior_summary: Some(&self.state.markdown),
            instruction: Some(&instruction),
            model: self.model.as_deref(),
            reasoning: self.reasoning.as_deref(),
        })?;

        let summary = summary_update_only(&self.state.markdown, &summary);
        if summary.is_empty() {
            return Ok(());
        }

        self.store.append_summary(&summary)?;
        self.state.markdown = self.store.load_summary()?;
        self.terminal.render_summary_update(&summary)?;
        Ok(())
    }

    fn summary_instruction(&self) -> String {
        let user_instruction = self.instruction.as_deref().unwrap_or(
            "Maintain a chronological, easy-to-scan live markdown summary. Write straight down in order, organized with useful section headings as topics shift. Use prose, short paragraphs, emphasis, inline code, and occasional bullets only when they genuinely improve readability. Avoid a rigid template like key points, decisions, and action items.",
        );

        format!(
            "\
{user_instruction}

Update the prior summary using the new transcript segment. Preserve continuity across arbitrary transcript boundaries. Do not invent details. Keep the summary chronological and organized with meaningful section headings. Return only the new markdown update to append to the existing summary. Do not repeat prior content."
        )
    }
}

impl TranscriptSink for LiveSummarizer {
    fn accept_transcript(&mut self, text: &str) -> Result<()> {
        self.accept_raw_text(text)
    }

    fn finish(self: Box<Self>) -> Result<()> {
        LiveSummarizer::finish(*self)
    }
}

impl TranscriptSinkBuilder {
    pub fn new(options: TranscriptOptions) -> Self {
        Self { options }
    }

    pub fn build(self) -> Result<Box<dyn TranscriptSink>> {
        if self.options.summary_path.is_some() {
            return Ok(Box::new(LiveSummarizer::new(self.options)?));
        }

        Ok(Box::new(RawTranscriptSink::new(self.options)?))
    }
}

impl RawTranscriptSink {
    fn new(options: TranscriptOptions) -> Result<Self> {
        let store = SummaryStore::new(None, options.raw_transcript_path)?;
        Ok(Self {
            store,
            terminal: TerminalRenderer::new(options.render_terminal),
        })
    }
}

impl TranscriptSink for RawTranscriptSink {
    fn accept_transcript(&mut self, text: &str) -> Result<()> {
        self.terminal.render_raw(text)?;
        self.store.append_raw(text)
    }

    fn finish(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

impl SummaryStore {
    fn new(summary_path: Option<PathBuf>, raw_transcript_path: Option<PathBuf>) -> Result<Self> {
        if let Some(summary_path) = &summary_path {
            ensure_parent_dir(summary_path)?;
        }
        if let Some(raw_path) = &raw_transcript_path {
            ensure_parent_dir(raw_path)?;
            fs::write(raw_path, "")
                .with_context(|| format!("failed to initialize {}", raw_path.display()))?;
        }

        Ok(Self {
            summary_path,
            raw_transcript_path,
        })
    }

    fn load_summary(&self) -> Result<String> {
        let Some(summary_path) = &self.summary_path else {
            return Ok(String::new());
        };

        if !summary_path.exists() {
            return Ok(String::new());
        }

        fs::read_to_string(summary_path)
            .with_context(|| format!("failed to read {}", summary_path.display()))
    }

    fn append_summary(&self, markdown: &str) -> Result<()> {
        let Some(path) = &self.summary_path else {
            return Ok(());
        };

        let markdown = markdown.trim();
        if markdown.is_empty() {
            return Ok(());
        }

        let needs_separator = path.exists()
            && fs::metadata(path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .len()
                > 0;

        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;

        if needs_separator {
            file.write_all(b"\n\n")
                .with_context(|| format!("failed to write {}", path.display()))?;
        }

        file.write_all(markdown.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))
    }

    fn append_raw(&self, text: &str) -> Result<()> {
        let Some(path) = &self.raw_transcript_path else {
            return Ok(());
        };

        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let text = format_transcript_text(text);
        file.write_all(text.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))
    }
}

impl TerminalRenderer {
    fn new(render_terminal: bool) -> Self {
        Self {
            render_terminal,
            markdown_options: MarkdownOptions {
                syntax_highlight: true,
                width: Some(100),
                code_bg: false,
            },
        }
    }

    fn render_raw(&self, text: &str) -> Result<()> {
        if !self.render_terminal {
            return Ok(());
        }

        let text = format_transcript_text(text);
        print!("\x1b[90m\x1b[3m{text}\x1b[0m");
        std::io::stdout()
            .flush()
            .context("failed to flush raw transcript")
    }

    fn render_summary_update(&self, update: &str) -> Result<()> {
        if !self.render_terminal {
            return Ok(());
        }

        let rendered = render(update, &self.markdown_options);
        println!("\n{rendered}");
        Ok(())
    }
}

impl TranscriptSegmenter {
    fn new(config: SegmenterConfig) -> Self {
        Self {
            config,
            pending: String::new(),
            segment_started_at: None,
            next_segment_id: 1,
        }
    }

    fn push(&mut self, text: &str, now: Instant) -> Option<SummarySegment> {
        if text.trim().is_empty() {
            return None;
        }

        if self.segment_started_at.is_none() {
            self.segment_started_at = Some(now);
        }
        append_normalized(&mut self.pending, text);

        if self.should_emit(now) {
            return self.emit(false);
        }

        None
    }

    fn flush(&mut self) -> Option<SummarySegment> {
        self.emit(true)
    }

    fn should_emit(&self, now: Instant) -> bool {
        let stable_chars = non_whitespace_chars(stable_prefix(&self.pending).0);
        if stable_chars == 0 {
            return false;
        }

        let elapsed = self
            .segment_started_at
            .map(|started_at| now.saturating_duration_since(started_at))
            .unwrap_or_default();

        stable_chars >= self.config.max_chars
            || (elapsed >= self.config.target_interval && stable_chars >= self.config.min_chars)
            || (elapsed >= self.config.force_interval
                && stable_chars >= self.config.force_min_chars)
    }

    fn emit(&mut self, force: bool) -> Option<SummarySegment> {
        let (stable, carryover) = if force {
            (self.pending.trim(), "")
        } else {
            let (stable, carryover) = stable_prefix(&self.pending);
            let carryover = trim_carryover(carryover, self.config.carryover_chars);
            (stable, carryover)
        };

        if stable.trim().is_empty() {
            return None;
        }

        let segment = SummarySegment {
            id: self.next_segment_id,
            raw_text: stable.trim().to_owned(),
        };
        self.next_segment_id += 1;

        self.pending = carryover.trim().to_owned();
        self.segment_started_at = (!self.pending.is_empty()).then_some(Instant::now());
        Some(segment)
    }
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

fn append_normalized(buffer: &mut String, text: &str) {
    let text = normalize_for_segment(text);
    if text.is_empty() {
        return;
    }

    if !buffer.is_empty() && !buffer.ends_with(char::is_whitespace) {
        buffer.push(' ');
    }
    buffer.push_str(&text);
}

fn normalize_for_segment(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_transcript_text(text: &str) -> String {
    let normalized = normalize_for_segment(text);
    if normalized.is_empty() {
        return normalized;
    }

    let mut output = String::with_capacity(normalized.len() + 2);
    let mut chars = normalized.chars().peekable();

    while let Some(ch) = chars.next() {
        output.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            while matches!(chars.peek(), Some(next) if next.is_whitespace()) {
                chars.next();
            }
            if chars.peek().is_some() {
                output.push('\n');
            }
        } else if ch == ',' && matches!(chars.peek(), Some(next) if !next.is_whitespace()) {
            output.push(' ');
        }
    }

    output.push('\n');
    output
}

fn stable_prefix(text: &str) -> (&str, &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return ("", "");
    }

    let Some(boundary) = last_sentence_boundary(trimmed) else {
        return ("", trimmed);
    };

    trimmed.split_at(boundary)
}

fn last_sentence_boundary(text: &str) -> Option<usize> {
    let mut last = None;
    for (index, ch) in text.char_indices() {
        if matches!(ch, '.' | '!' | '?') {
            last = Some(index + ch.len_utf8());
        }
    }
    last
}

fn trim_carryover(text: &str, max_chars: usize) -> &str {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text;
    }

    let mut start = 0;
    for (index, _) in text.char_indices().rev().take(max_chars) {
        start = index;
    }
    text[start..].trim_start()
}

fn non_whitespace_chars(text: &str) -> usize {
    text.chars().filter(|ch| !ch.is_whitespace()).count()
}

fn summary_update_only(prior_summary: &str, model_output: &str) -> String {
    let output = model_output.trim();
    if output.is_empty() || output == "<!-- no update -->" {
        return String::new();
    }

    let prior = prior_summary.trim();
    if prior.is_empty() {
        return output.to_owned();
    }

    if let Some(update) = output.strip_prefix(prior) {
        return trim_summary_separator(update);
    }

    strip_common_line_prefix(prior, output)
}

fn trim_summary_separator(text: &str) -> String {
    text.trim_start_matches(|ch: char| ch.is_whitespace())
        .to_owned()
}

fn strip_common_line_prefix(prior_summary: &str, model_output: &str) -> String {
    let prior_lines: Vec<&str> = prior_summary.lines().collect();
    let output_lines: Vec<&str> = model_output.lines().collect();
    let mut shared_lines = 0;

    while shared_lines < prior_lines.len()
        && shared_lines < output_lines.len()
        && prior_lines[shared_lines].trim_end() == output_lines[shared_lines].trim_end()
    {
        shared_lines += 1;
    }

    if shared_lines == 0 {
        return model_output.trim().to_owned();
    }

    output_lines[shared_lines..].join("\n").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::SummaryRequest;

    struct FakeProvider;

    impl LanguageModelProvider for FakeProvider {
        fn authenticate(&self) -> Result<()> {
            Ok(())
        }

        fn auth_status(&self) -> Result<String> {
            Ok("ok".to_owned())
        }

        fn ensure_authenticated(&self) -> Result<()> {
            Ok(())
        }

        fn summarize(&self, request: SummaryRequest<'_>) -> Result<String> {
            let prior = request.prior_summary.unwrap_or("").trim();
            if prior.is_empty() {
                Ok(format!("- {}", request.transcript))
            } else {
                Ok(format!("{prior}\n- {}", request.transcript))
            }
        }
    }

    #[test]
    fn segmenter_waits_for_sentence_boundary() {
        let mut segmenter = TranscriptSegmenter::new(SegmenterConfig {
            target_interval: Duration::from_secs(30),
            force_interval: Duration::from_secs(60),
            min_chars: 5,
            force_min_chars: 1,
            max_chars: 50,
            carryover_chars: 20,
        });
        let now = Instant::now();

        assert!(segmenter.push("hello world", now).is_none());
        let segment = segmenter
            .push(" and done. next partial", now + Duration::from_secs(30))
            .unwrap();

        assert_eq!(segment.raw_text, "hello world and done.");
        assert_eq!(segmenter.pending, "next partial");
    }

    #[test]
    fn segmenter_force_flushes_partial_text() {
        let mut segmenter = TranscriptSegmenter::new(SegmenterConfig::default());

        segmenter.push("partial thought without punctuation", Instant::now());
        let segment = segmenter.flush().unwrap();

        assert_eq!(segment.raw_text, "partial thought without punctuation");
    }

    #[test]
    fn segmenter_uses_fast_char_gate() {
        let mut segmenter = TranscriptSegmenter::new(SegmenterConfig {
            target_interval: Duration::from_secs(30),
            force_interval: Duration::from_secs(60),
            min_chars: 300,
            force_min_chars: 80,
            max_chars: 10,
            carryover_chars: 20,
        });

        let segment = segmenter
            .push("one sentence is long enough.", Instant::now())
            .unwrap();
        assert_eq!(segment.raw_text, "one sentence is long enough.");
    }

    #[test]
    fn live_summarizer_saves_summary_and_optional_raw_transcript() {
        let dir =
            std::env::temp_dir().join(format!("palantwire-summary-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        let summary_path = dir.join("summary.md");
        let raw_path = dir.join("raw.txt");

        let mut summarizer = LiveSummarizer::with_provider(
            TranscriptOptions {
                summary_path: Some(summary_path.clone()),
                raw_transcript_path: Some(raw_path.clone()),
                instruction: None,
                model: None,
                reasoning: None,
                render_terminal: false,
            },
            Box::new(FakeProvider),
        )
        .unwrap();

        summarizer
            .accept_raw_text("The first stable point is done.")
            .unwrap();
        summarizer.finish().unwrap();

        assert_eq!(
            fs::read_to_string(summary_path).unwrap(),
            "- The first stable point is done."
        );
        assert_eq!(
            fs::read_to_string(raw_path).unwrap(),
            "The first stable point is done.\n"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn summary_store_appends_updates_with_spacing() {
        let dir =
            std::env::temp_dir().join(format!("palantwire-summary-append-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        let summary_path = dir.join("summary.md");

        let store = SummaryStore::new(Some(summary_path.clone()), None).unwrap();
        store.append_summary("# First update").unwrap();
        store.append_summary("## Second update").unwrap();

        assert_eq!(
            fs::read_to_string(summary_path).unwrap(),
            "# First update\n\n## Second update"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn transcript_format_puts_sentences_on_new_lines() {
        assert_eq!(
            format_transcript_text("First sentence. Second sentence!Third? final"),
            "First sentence.\nSecond sentence!\nThird?\nfinal\n"
        );
    }

    #[test]
    fn summary_update_only_strips_full_summary_response() {
        let prior = "# Existing\n\nThe old summary.";
        let full_response = "# Existing\n\nThe old summary.\n\n## New\n\nFresh detail.";

        assert_eq!(
            summary_update_only(prior, full_response),
            "## New\n\nFresh detail."
        );
    }

    #[test]
    fn summary_update_only_strips_common_line_prefix() {
        let prior = "# Existing\n\nThe old summary.";
        let full_response = "# Existing \n\nThe old summary.\n\n## New\n\nFresh detail.";

        assert_eq!(
            summary_update_only(prior, full_response),
            "## New\n\nFresh detail."
        );
    }
}
