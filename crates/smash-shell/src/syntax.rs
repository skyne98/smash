use crate::textbox::TextBoxLanguage;
use crate::theme::SmashTheme;
use linguist::{
    DetectedLanguage, definitions, detect_language_by_extension, detect_language_by_filename,
    disambiguate, utils::matches_pattern,
};
use linguist_types::HeuristicRule;
use ratatui::style::{Color, Modifier, Style};
use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    FontStyle as SyntectFontStyle, Style as SyntectStyle, Theme, ThemeSet,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use tokio::runtime::{Builder, Runtime};
use tokio::task::spawn_blocking;
use tokio::time::sleep;

#[cfg(test)]
const SYNTAX_DEBOUNCE: Duration = Duration::from_millis(10);
#[cfg(not(test))]
const SYNTAX_DEBOUNCE: Duration = Duration::from_millis(75);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SyntaxThemeKind {
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyntaxRequest {
    pub revision: u64,
    pub theme_kind: SyntaxThemeKind,
    pub title: String,
    pub path_hint: Option<String>,
    pub language: TextBoxLanguage,
    pub lines: Vec<String>,
}

impl SyntaxRequest {
    pub(crate) fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.revision.hash(&mut hasher);
        self.theme_kind.hash(&mut hasher);
        self.title.hash(&mut hasher);
        self.path_hint.hash(&mut hasher);
        self.language.hash(&mut hasher);
        self.lines.hash(&mut hasher);
        hasher.finish()
    }

    fn joined_text(&self) -> String {
        self.lines.join("\n")
    }

    fn detection_hint(&self) -> Option<&str> {
        self.path_hint
            .as_deref()
            .filter(|hint| !hint.trim().is_empty())
            .or_else(|| {
                (!self.title.trim().is_empty() && looks_like_filename(&self.title))
                    .then_some(self.title.as_str())
            })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SyntaxSnapshot {
    #[cfg_attr(not(test), allow(dead_code))]
    pub revision: u64,
    pub fingerprint: u64,
    pub language_label: String,
    pub line_styles: Vec<Vec<Style>>,
}

#[derive(Clone)]
pub(crate) struct SyntaxWorker {
    state: Arc<Mutex<SyntaxWorkerState>>,
}

#[derive(Default)]
struct SyntaxWorkerState {
    latest_request: Option<SyntaxRequest>,
    generation: u64,
    in_flight_fingerprint: Option<u64>,
    latest_snapshot: Option<Arc<SyntaxSnapshot>>,
}

struct SyntaxAssets {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

#[derive(Clone)]
struct SyntaxSelection<'a> {
    syntax: &'a SyntaxReference,
    label: String,
}

impl SyntaxWorker {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SyntaxWorkerState::default())),
        }
    }

    pub(crate) fn schedule(&self, request: SyntaxRequest) {
        let fingerprint = request.fingerprint();
        let generation = {
            let mut state = self.state.lock().expect("syntax worker lock poisoned");
            if state.in_flight_fingerprint == Some(fingerprint)
                || state
                    .latest_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.fingerprint == fingerprint)
                || state
                    .latest_request
                    .as_ref()
                    .is_some_and(|pending| pending.fingerprint() == fingerprint)
            {
                return;
            }

            state.latest_request = Some(request);
            state.generation += 1;
            state.generation
        };

        let worker = self.clone();
        syntax_runtime().spawn(async move {
            sleep(SYNTAX_DEBOUNCE).await;
            worker.process_generation(generation).await;
        });
    }

    pub(crate) fn latest_snapshot(&self) -> Option<Arc<SyntaxSnapshot>> {
        self.state
            .lock()
            .expect("syntax worker lock poisoned")
            .latest_snapshot
            .clone()
    }

    async fn process_generation(&self, generation: u64) {
        let request = {
            let mut state = self.state.lock().expect("syntax worker lock poisoned");
            if state.generation != generation {
                return;
            }

            let Some(request) = state.latest_request.clone() else {
                return;
            };
            let fingerprint = request.fingerprint();
            if state.in_flight_fingerprint == Some(fingerprint)
                || state
                    .latest_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.fingerprint == fingerprint)
            {
                return;
            }

            state.in_flight_fingerprint = Some(fingerprint);
            request
        };

        let fingerprint = request.fingerprint();
        let request_for_worker = request.clone();
        let snapshot = spawn_blocking(move || highlight_request_sync(&request_for_worker))
            .await
            .unwrap_or_else(|_| fallback_snapshot(&request));
        let snapshot = Arc::new(snapshot);

        let mut state = self.state.lock().expect("syntax worker lock poisoned");
        if state.in_flight_fingerprint == Some(fingerprint) {
            state.in_flight_fingerprint = None;
        }
        if state
            .latest_request
            .as_ref()
            .is_some_and(|pending| pending.fingerprint() == fingerprint)
        {
            state.latest_snapshot = Some(snapshot);
        }
    }
}

pub(crate) fn theme_kind_for(theme: &SmashTheme) -> SyntaxThemeKind {
    let Color::Rgb(r, g, b) = theme.background else {
        return SyntaxThemeKind::Dark;
    };
    let luminance = (0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b)) / 255.0;
    if luminance < 0.5 {
        SyntaxThemeKind::Dark
    } else {
        SyntaxThemeKind::Light
    }
}

pub(crate) fn detect_language_label(request: &SyntaxRequest) -> String {
    let selection = select_syntax(syntax_assets(), request);
    selection.label
}

pub(crate) fn highlight_request_sync(request: &SyntaxRequest) -> SyntaxSnapshot {
    let assets = syntax_assets();
    let selection = select_syntax(assets, request);
    let fingerprint = request.fingerprint();

    let line_styles = if selection.syntax.name == "Plain Text" {
        request
            .lines
            .iter()
            .map(|line| vec![Style::default(); line.chars().count()])
            .collect()
    } else {
        let theme = theme_for_kind(&assets.themes, request.theme_kind);
        let mut highlighter = HighlightLines::new(selection.syntax, theme);
        request
            .lines
            .iter()
            .map(|line| highlight_line_styles(&mut highlighter, &assets.syntaxes, line))
            .collect()
    };

    SyntaxSnapshot {
        revision: request.revision,
        fingerprint,
        language_label: selection.label,
        line_styles,
    }
}

fn fallback_snapshot(request: &SyntaxRequest) -> SyntaxSnapshot {
    SyntaxSnapshot {
        revision: request.revision,
        fingerprint: request.fingerprint(),
        language_label: "Plain Text".to_string(),
        line_styles: request
            .lines
            .iter()
            .map(|line| vec![Style::default(); line.chars().count()])
            .collect(),
    }
}

fn highlight_line_styles(
    highlighter: &mut HighlightLines<'_>,
    syntaxes: &SyntaxSet,
    line: &str,
) -> Vec<Style> {
    match highlighter.highlight_line(line, syntaxes) {
        Ok(ranges) => {
            let mut styles = Vec::with_capacity(line.chars().count());
            for (style, segment) in ranges {
                let ratatui_style = ratatui_style_from_syntect(style);
                styles.extend(std::iter::repeat_n(ratatui_style, segment.chars().count()));
            }
            styles
        }
        Err(_) => vec![Style::default(); line.chars().count()],
    }
}

fn ratatui_style_from_syntect(style: SyntectStyle) -> Style {
    let mut ratatui_style = Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));

    if style.font_style.contains(SyntectFontStyle::BOLD) {
        ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(SyntectFontStyle::ITALIC) {
        ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(SyntectFontStyle::UNDERLINE) {
        ratatui_style = ratatui_style.add_modifier(Modifier::UNDERLINED);
    }

    ratatui_style
}

fn select_syntax<'a>(assets: &'a SyntaxAssets, request: &SyntaxRequest) -> SyntaxSelection<'a> {
    if let Some(selection) = select_syntax_from_override(assets, request.language) {
        return selection;
    }

    let detected = detect_with_linguist(request);
    for language in &detected {
        if let Some(selection) = select_syntax_from_detected(assets, language) {
            return selection;
        }
    }

    if let Some(hint) = request.detection_hint()
        && let Some(selection) = select_syntax_from_hint(assets, hint)
    {
        return selection;
    }

    for language in detect_with_linguist_content(request) {
        if let Some(selection) = select_syntax_from_detected(assets, &language) {
            return selection;
        }
    }

    if let Some(first_line) = request
        .lines
        .iter()
        .map(String::as_str)
        .find(|line| !line.trim().is_empty())
        && let Some(syntax) = assets.syntaxes.find_syntax_by_first_line(first_line)
    {
        return SyntaxSelection {
            syntax,
            label: syntax.name.to_string(),
        };
    }

    SyntaxSelection {
        syntax: assets.syntaxes.find_syntax_plain_text(),
        label: "Plain Text".to_string(),
    }
}

fn select_syntax_from_override<'a>(
    assets: &'a SyntaxAssets,
    language: TextBoxLanguage,
) -> Option<SyntaxSelection<'a>> {
    let (label, tokens): (&str, &[&str]) = match language {
        TextBoxLanguage::Auto => return None,
        TextBoxLanguage::PlainText => ("Plain Text", &[]),
        TextBoxLanguage::Rust => ("Rust", &["rs", "rust"]),
        TextBoxLanguage::Markdown => ("Markdown", &["md", "markdown"]),
        TextBoxLanguage::Json => ("JSON", &["json"]),
        TextBoxLanguage::Toml => ("TOML", &["toml"]),
        TextBoxLanguage::Yaml => ("YAML", &["yaml", "yml"]),
        TextBoxLanguage::Shell => ("Shell", &["sh", "bash", "zsh", "shell"]),
    };

    if language == TextBoxLanguage::PlainText {
        return Some(SyntaxSelection {
            syntax: assets.syntaxes.find_syntax_plain_text(),
            label: label.to_string(),
        });
    }

    for token in tokens {
        if let Some(syntax) = assets.syntaxes.find_syntax_by_token(token) {
            return Some(SyntaxSelection {
                syntax,
                label: label.to_string(),
            });
        }
    }

    Some(SyntaxSelection {
        syntax: assets.syntaxes.find_syntax_plain_text(),
        label: label.to_string(),
    })
}

fn detect_with_linguist(request: &SyntaxRequest) -> Vec<DetectedLanguage> {
    let Some(hint) = request.detection_hint() else {
        return Vec::new();
    };

    if let Ok(filename_matches) = detect_language_by_filename(hint) {
        if !filename_matches.is_empty() {
            return filename_matches;
        }
    }

    if let Ok(extension_matches) = detect_language_by_extension(hint) {
        if extension_matches.len() > 1 {
            if let Ok(disambiguated) = disambiguate(hint, &request.joined_text())
                && !disambiguated.is_empty()
            {
                return disambiguated;
            }
        }

        if !extension_matches.is_empty() {
            return extension_matches;
        }
    }

    Vec::new()
}

fn detect_with_linguist_content(request: &SyntaxRequest) -> Vec<DetectedLanguage> {
    let content = request.joined_text();
    if content.trim().is_empty() {
        return Vec::new();
    }
    let content = strip_utf8_bom(&content);

    let mut candidates: HashMap<&'static str, (usize, usize, DetectedLanguage)> = HashMap::new();
    for disambiguation in &definitions::HEURISTICS.disambiguations {
        let mut matched_score = 0;
        let mut matched_languages = Vec::new();

        for rule in &disambiguation.rules {
            if !evaluate_heuristic_rule(rule, content) {
                continue;
            }

            matched_score = heuristic_rule_score(rule);
            if let Some(language_names) = &rule.language {
                matched_languages.extend(language_names.iter().filter_map(|language_name| {
                    definitions::LANGUAGES
                        .get(language_name)
                        .map(|definition| DetectedLanguage {
                            name: language_name.as_str(),
                            definition,
                        })
                }));
            }
            break;
        }

        for language in matched_languages {
            let entry = candidates
                .entry(language.name)
                .or_insert_with(|| (0, 0, language.clone()));
            entry.0 += matched_score;
            entry.1 += 1;
        }
    }

    let mut ranked = candidates.into_values().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then(right.1.cmp(&left.1))
            .then(left.2.name.cmp(right.2.name))
    });
    if let Some((best_score, best_matches, _)) = ranked.first()
        && ranked
            .get(1)
            .is_some_and(|(runner_up_score, runner_up_matches, _)| {
                runner_up_score == best_score && runner_up_matches == best_matches
            })
    {
        return Vec::new();
    }
    ranked
        .into_iter()
        .take(1)
        .map(|(_, _, language)| language)
        .collect()
}

fn strip_utf8_bom(content: &str) -> &str {
    content.strip_prefix('\u{FEFF}').unwrap_or(content)
}

fn evaluate_heuristic_rule(rule: &HeuristicRule, content: &str) -> bool {
    if let Some(and_rules) = &rule.and
        && !and_rules
            .iter()
            .all(|sub_rule| evaluate_heuristic_rule(sub_rule, content))
    {
        return false;
    }

    if let Some(named_pattern) = &rule.named_pattern {
        let patterns = definitions::HEURISTICS
            .named_patterns
            .get(named_pattern)
            .unwrap_or_else(|| panic!("missing bundled linguist named pattern: {named_pattern}"));
        if !matches_pattern(patterns, content).expect("invalid bundled linguist named pattern") {
            return false;
        }
    }

    if let Some(patterns) = &rule.pattern
        && !matches_pattern(patterns, content).expect("invalid bundled linguist heuristic")
    {
        return false;
    }

    if let Some(negative_patterns) = &rule.negative_pattern
        && matches_pattern(negative_patterns, content).expect("invalid bundled linguist heuristic")
    {
        return false;
    }

    true
}

fn heuristic_rule_score(rule: &HeuristicRule) -> usize {
    let mut score = 1;
    if let Some(patterns) = &rule.pattern {
        score += patterns.len() * 2;
    }
    if let Some(negative_patterns) = &rule.negative_pattern {
        score += negative_patterns.len();
    }
    if rule.named_pattern.is_some() {
        score += 2;
    }
    if let Some(and_rules) = &rule.and {
        score += and_rules.iter().map(heuristic_rule_score).sum::<usize>();
    }
    score
}

fn select_syntax_from_detected<'a>(
    assets: &'a SyntaxAssets,
    language: &DetectedLanguage,
) -> Option<SyntaxSelection<'a>> {
    let mut candidates = Vec::new();

    candidates.push(language.name.to_string());
    if let Some(fs_name) = language.definition.fs_name.as_ref() {
        candidates.push(fs_name.clone());
    }
    if let Some(aliases) = language.definition.aliases.as_ref() {
        candidates.extend(aliases.clone());
    }

    let mut seen = HashSet::new();
    for candidate in candidates {
        let normalized = candidate.to_ascii_lowercase();
        if !seen.insert(normalized) {
            continue;
        }
        if let Some(syntax) = assets.syntaxes.find_syntax_by_token(&candidate) {
            return Some(SyntaxSelection {
                syntax,
                label: language.name.to_string(),
            });
        }
        if let Some(syntax) = assets.syntaxes.find_syntax_by_name(&candidate) {
            return Some(SyntaxSelection {
                syntax,
                label: language.name.to_string(),
            });
        }
    }

    None
}

fn select_syntax_from_hint<'a>(
    assets: &'a SyntaxAssets,
    hint: &str,
) -> Option<SyntaxSelection<'a>> {
    for token in extension_tokens(hint) {
        if let Some(syntax) = assets.syntaxes.find_syntax_by_token(&token) {
            return Some(SyntaxSelection {
                label: syntax.name.to_string(),
                syntax,
            });
        }
    }

    None
}

fn extension_tokens(hint: &str) -> Vec<String> {
    let file_name = Path::new(hint)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(hint);
    let pieces: Vec<&str> = file_name.split('.').collect();
    if pieces.len() < 2 {
        return Vec::new();
    }

    let mut tokens = Vec::new();
    for idx in 1..pieces.len() {
        let token = pieces[idx..].join(".");
        if !token.is_empty() {
            tokens.push(token);
        }
    }
    tokens
}

fn looks_like_filename(value: &str) -> bool {
    value.contains('.') || value.contains('/') || value.contains('\\')
}

fn theme_for_kind<'a>(themes: &'a ThemeSet, kind: SyntaxThemeKind) -> &'a Theme {
    let name = match kind {
        SyntaxThemeKind::Dark => "base16-ocean.dark",
        SyntaxThemeKind::Light => "InspiredGitHub",
    };

    themes.themes.get(name).unwrap_or_else(|| {
        themes
            .themes
            .values()
            .next()
            .expect("syntect theme set is empty")
    })
}

fn syntax_assets() -> &'static SyntaxAssets {
    static ASSETS: OnceLock<SyntaxAssets> = OnceLock::new();
    ASSETS.get_or_init(|| SyntaxAssets {
        syntaxes: SyntaxSet::load_defaults_nonewlines(),
        themes: ThemeSet::load_defaults(),
    })
}

fn syntax_runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(4)
            .enable_time()
            .build()
            .expect("failed to build syntax runtime")
    })
}
