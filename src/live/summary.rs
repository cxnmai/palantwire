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
    last_segment_id: u64,
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
            target_interval: Duration::from_secs(30),
            force_interval: Duration::from_secs(60),
            min_chars: 300,
            force_min_chars: 80,
            max_chars: 900,
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
        store.write_summary("")?;

        Ok(Self {
            provider,
            segmenter: TranscriptSegmenter::new(SegmenterConfig::default()),
            store,
            state: SummaryState::default(),
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
        let summary = self.provider.summarize(SummaryRequest {
            transcript: &segment.raw_text,
            prior_summary: Some(&self.state.markdown),
            instruction: Some(&instruction),
            model: self.model.as_deref(),
            reasoning: self.reasoning.as_deref(),
        })?;

        self.state.markdown = summary;
        self.state.last_segment_id = segment.id;
        self.store.write_summary(&self.state.markdown)?;
        self.terminal.render_summary(&self.state.markdown)?;
        Ok(())
    }

    fn summary_instruction(&self) -> String {
        let user_instruction = self.instruction.as_deref().unwrap_or(
            "Maintain a concise live markdown summary with key points, decisions, and action items.",
        );

        format!(
            "\
{user_instruction}

Update the prior summary using the new transcript segment. Preserve continuity across arbitrary transcript boundaries. Do not invent details. Return the complete updated markdown summary only."
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

    fn write_summary(&self, markdown: &str) -> Result<()> {
        let Some(summary_path) = &self.summary_path else {
            return Ok(());
        };

        fs::write(summary_path, markdown)
            .with_context(|| format!("failed to write {}", summary_path.display()))
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

        print!("\x1b[90m\x1b[3m{text}\x1b[0m");
        std::io::stdout()
            .flush()
            .context("failed to flush raw transcript")
    }

    fn render_summary(&self, markdown: &str) -> Result<()> {
        if !self.render_terminal {
            return Ok(());
        }

        let rendered = render(markdown, &self.markdown_options);
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
    let text = normalize_spaces(text);
    if text.is_empty() {
        return;
    }

    if !buffer.is_empty() && !buffer.ends_with(char::is_whitespace) {
        buffer.push(' ');
    }
    buffer.push_str(&text);
}

fn normalize_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
            "The first stable point is done."
        );

        let _ = fs::remove_dir_all(dir);
    }
}
