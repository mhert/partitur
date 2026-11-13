//! Measures how fast the Session tab handles a long transcript.
//!
//! `--write` generates the fixture `transcript_10k.jsonl`: 10,000 updates, one JSON
//! [`Update`] per line, produced deterministically from a fixed seed. Without arguments the
//! example reads the fixture and prints the median of five runs, in milliseconds, of:
//!
//! - `from_updates`: rebuilding the [`Transcript`] from all updates.
//! - `markdown_sync_full`: parsing every agent answer from scratch with [`MarkdownCache::sync`].
//! - `incremental_step`: applying one more agent text chunk and syncing again, which parses
//!   only the grown tail (`apply` and `sync` are listed separately).
//! - `session_view_build`: building the Session tab's element tree with
//!   `session_view::view`. The cache inside [`SessionState`] is private, so agent answers are
//!   built as plain text here.
//! - `markdown_view_build`: building the Markdown elements of every agent answer from the
//!   synced cache, the part `session_view_build` leaves out.
//! - `session_view_layout`: building the Session tab and laying it out once in a simulator
//!   with the app's fonts, which is what opening the session costs. Fonts are loaded per
//!   simulator, so this runs once per process and its time includes loading them.
//!
//! ```text
//! cargo run --release -p partitur-iced --example transcript_bench -- --write
//! cargo run --release -p partitur-iced --example transcript_bench
//! ```

use std::borrow::Cow;
use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::widget::{Column, markdown};
use iced::{Pixels, Settings};
use iced_test::Simulator;
use partitur_core::testing::single_claude;
use partitur_core::{
    Entry, SessionId, SessionViewModel, Status, ToolCallId, ToolContent, ToolKind, ToolStatus,
    Transcript, Update,
};
use partitur_iced::markdown_cache::MarkdownCache;
use partitur_iced::session_view::{self, SessionState};
use partitur_iced::{style, theme};

/// The seed the fixture is generated from.
const SEED: u64 = 0x5EED_CAFE_F00D_0001;

/// Every turn has exactly 20 updates, so 500 turns make 10,000 with the 60/15/20/5 mix:
/// 12 agent chunks, 3 thought chunks, 4 tool call updates (two calls) and 1 user message.
const TURNS: usize = 500;
const AGENT_CHUNKS_PER_TURN: usize = 12;

/// A fenced code block replaces three consecutive chunks about every this many chunks.
const FENCE_EVERY: usize = 200;

/// How often each measurement runs.
const RUNS: usize = 5;

const WORDS: [&str; 32] = [
    "the",
    "session",
    "transcript",
    "agent",
    "reads",
    "file",
    "project",
    "change",
    "update",
    "streams",
    "answer",
    "prompt",
    "tool",
    "result",
    "branch",
    "commit",
    "cache",
    "parse",
    "render",
    "entry",
    "chunk",
    "message",
    "window",
    "worktree",
    "backend",
    "adapter",
    "usage",
    "context",
    "review",
    "pattern",
    "module",
    "error",
];

const TOOL_KINDS: [ToolKind; 4] = [
    ToolKind::Read,
    ToolKind::Edit,
    ToolKind::Search,
    ToolKind::Execute,
];

/// A xorshift64* generator: tiny, and the same sequence on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..bound`; `0` when `bound` is `0`.
    fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).unwrap_or(u64::MAX).max(1);
        usize::try_from((self.next() >> 33) % bound).unwrap_or(0)
    }

    /// A number in `low..=high`.
    fn between(&mut self, low: usize, high: usize) -> usize {
        low + self.below(high - low + 1)
    }

    fn word(&mut self) -> &'static str {
        WORDS
            .get(self.below(WORDS.len()))
            .copied()
            .unwrap_or("word")
    }
}

/// Builds the fixture's updates.
struct Generator {
    rng: Rng,
    updates: Vec<Update>,
    agent_chunks: usize,
    next_fence_at: usize,
    tool_calls: usize,
}

impl Generator {
    fn new() -> Self {
        Self {
            rng: Rng(SEED),
            updates: Vec::new(),
            agent_chunks: 0,
            next_fence_at: FENCE_EVERY,
            tool_calls: 0,
        }
    }

    fn generate(mut self) -> Vec<Update> {
        for _ in 0..TURNS {
            self.turn();
        }
        self.updates
    }

    /// One user message, three thought chunks, two tool calls and twelve agent chunks, ending
    /// in agent text so a stream can continue at the end.
    fn turn(&mut self) {
        let message = self.prose(30, 120, false);
        self.updates.push(Update::UserText(message));
        // Thoughts either come first or are split around the first tool call.
        let thoughts_before = if self.rng.below(2) == 0 { 3 } else { 1 };
        self.thoughts(thoughts_before);
        self.tool_call();
        self.thoughts(3 - thoughts_before);
        let first = self.rng.between(3, AGENT_CHUNKS_PER_TURN - 3);
        self.agent_run(first);
        self.tool_call();
        self.agent_run(AGENT_CHUNKS_PER_TURN - first);
    }

    fn thoughts(&mut self, count: usize) {
        for _ in 0..count {
            let chunk = self.prose(20, 80, false);
            self.updates.push(Update::AgentThought(chunk));
        }
    }

    /// A tool call and the update that completes it with text output.
    fn tool_call(&mut self) {
        self.tool_calls += 1;
        let id = ToolCallId::new(format!("tool-{}", self.tool_calls));
        let kind = TOOL_KINDS
            .get(self.rng.below(TOOL_KINDS.len()))
            .copied()
            .unwrap_or(ToolKind::Other);
        let title = format!("{} src/{}.rs", self.rng.word(), self.rng.word());
        let output = self.prose(50, 200, false);
        self.updates.push(Update::ToolCall {
            id: id.clone(),
            title,
            kind,
            status: ToolStatus::InProgress,
            content: Vec::new(),
            locations: vec![PathBuf::from(format!("src/{}.rs", self.rng.word()))],
        });
        self.updates.push(Update::ToolCallUpdate {
            id,
            title: None,
            kind: None,
            status: Some(ToolStatus::Completed),
            content: Some(vec![ToolContent::Text(output)]),
            raw_output: None,
        });
    }

    /// `count` agent chunks; a fenced code block takes three of them when one is due.
    fn agent_run(&mut self, count: usize) {
        let mut done = 0;
        while done < count {
            let fence_due = self.agent_chunks >= self.next_fence_at;
            let chunks = if fence_due && count - done >= 3 {
                self.next_fence_at += FENCE_EVERY;
                fence_chunks()
            } else {
                vec![self.prose(20, 80, true)]
            };
            done += chunks.len();
            self.agent_chunks += chunks.len();
            self.updates
                .extend(chunks.into_iter().map(Update::AgentText));
        }
    }

    /// Words up to a random length in `low..=high` bytes; with `markdown`, some words are
    /// bold or inline code, and a chunk sometimes ends a paragraph.
    fn prose(&mut self, low: usize, high: usize, markdown: bool) -> String {
        let target = self.rng.between(low, high);
        let mut text = String::new();
        loop {
            let word = self.rng.word();
            let token = match (markdown, self.rng.below(8)) {
                (true, 0) => format!("**{word}**"),
                (true, 1) => format!("`{word}`"),
                _ => word.to_owned(),
            };
            if text.len() >= low && text.len() + token.len() + 1 > target {
                break;
            }
            text.push_str(&token);
            text.push(' ');
        }
        if markdown && self.rng.below(10) == 0 {
            text.push('\n');
        }
        text
    }
}

/// The three chunks of a streamed fenced code block.
fn fence_chunks() -> Vec<String> {
    vec![
        "Here is an example:\n```rust\n".to_owned(),
        "fn example(value: u32) -> u32 {\n    value + 1\n}".to_owned(),
        "\n```\n\nThat shows the idea.\n\n".to_owned(),
    ]
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/transcript_10k.jsonl")
}

fn write_fixture() -> Result<(), Box<dyn Error>> {
    let mut lines = String::new();
    for update in Generator::new().generate() {
        lines.push_str(&serde_json::to_string(&update)?);
        lines.push('\n');
    }
    let path = fixture_path();
    fs::write(&path, &lines)?;
    println!("wrote {} ({} bytes)", path.display(), lines.len());
    Ok(())
}

fn read_fixture() -> Result<Vec<Update>, Box<dyn Error>> {
    let path = fixture_path();
    let lines = fs::read_to_string(&path).map_err(|error| {
        format!(
            "cannot read {}: {error}; generate it with `-- --write`",
            path.display()
        )
    })?;
    lines
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn model<'a>(id: &SessionId, transcript: &'a Transcript) -> SessionViewModel<'a> {
    let (backends, backend) = single_claude();
    SessionViewModel {
        id: id.clone(),
        breadcrumb: vec!["project".to_owned(), "session".to_owned()],
        status: Status::Running,
        attention: false,
        orchestrator: false,
        children: Vec::new(),
        repositories: Vec::new(),
        entries: transcript.entries(),
        revision: transcript.revision(),
        usage: None,
        model: None,
        effort: None,
        auto_allow: false,
        backends,
        backend,
        cwd: PathBuf::from("/work"),
        archived: None,
        child_defaults: None,
        approval: None,
        workflow: None,
    }
}

/// How long `work` takes, including dropping what it returns.
fn time<T>(work: impl FnOnce() -> T) -> Duration {
    let started = Instant::now();
    drop(black_box(work()));
    started.elapsed()
}

/// The median of `samples`, in milliseconds.
fn median_ms(mut samples: Vec<Duration>) -> f64 {
    samples.sort();
    samples
        .get(samples.len() / 2)
        .copied()
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}

fn report(name: &str, samples: Vec<Duration>) {
    println!("{name}: {:.3} ms", median_ms(samples));
}

fn benchmark() -> Result<(), Box<dyn Error>> {
    let updates = read_fixture()?;
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    println!("profile: {profile}, updates: {}", updates.len());

    report(
        "from_updates",
        (0..RUNS)
            .map(|_| time(|| Transcript::from_updates(&updates)))
            .collect(),
    );

    let mut transcript = Transcript::from_updates(&updates);
    let id = SessionId::new();
    let agents = transcript
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            Entry::Agent(text) => Some(text.len()),
            _ => None,
        })
        .collect::<Vec<_>>();
    println!(
        "entries: {}, agent entries: {}, agent bytes: {}",
        transcript.entries().len(),
        agents.len(),
        agents.iter().sum::<usize>()
    );

    report(
        "markdown_sync_full",
        (0..RUNS)
            .map(|_| {
                let mut cache = MarkdownCache::default();
                time(|| cache.sync(&model(&id, &transcript)))
            })
            .collect(),
    );

    if !matches!(transcript.entries().last(), Some(Entry::Agent(_))) {
        return Err("the fixture must end with agent text".into());
    }
    let mut cache = MarkdownCache::default();
    cache.sync(&model(&id, &transcript));
    let (mut apply, mut sync) = (Vec::new(), Vec::new());
    for run in 0..RUNS {
        let chunk = Update::AgentText(format!("one more chunk with **bold** text, number {run} "));
        apply.push(time(|| transcript.apply(&chunk)));
        sync.push(time(|| cache.sync(&model(&id, &transcript))));
    }
    let total = apply.iter().zip(&sync).map(|(a, s)| *a + *s).collect();
    report("incremental_step", total);
    report("  apply", apply);
    report("  sync", sync);

    let state = SessionState::default();
    let view_model = model(&id, &transcript);
    report(
        "session_view_build",
        (0..RUNS)
            .map(|_| time(|| session_view::view(&view_model, &state, &theme::DARK)))
            .collect(),
    );
    report(
        "markdown_view_build",
        (0..RUNS)
            .map(|_| {
                time(|| {
                    let answers = view_model
                        .entries
                        .iter()
                        .enumerate()
                        .filter_map(|(index, _)| cache.items(&id, index))
                        .map(|items| markdown::view(items, style::markdown(&theme::DARK)));
                    Column::<markdown::Uri>::with_children(answers)
                })
            })
            .collect(),
    );
    let settings = Settings {
        fonts: theme::FONTS.into_iter().map(Cow::Borrowed).collect(),
        default_font: theme::UI,
        default_text_size: Pixels(theme::TEXT),
        ..Settings::default()
    };
    report(
        "session_view_layout",
        vec![time(|| {
            Simulator::with_settings(
                settings,
                session_view::view(&view_model, &state, &theme::DARK),
            )
        })],
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().any(|argument| argument == "--write") {
        write_fixture()
    } else {
        benchmark()
    }
}
