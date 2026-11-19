//! Fuzzy search over the short strings of each session: title, project,
//! branch, first prompt and summary. Titles match fuzzily, so `tokn rfrsh` finds
//! "token refresh"; elsewhere each word of the query must appear intact.

use crate::model::Session;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

pub struct Hit {
    /// Index into the slice that was searched.
    pub index: usize,
    pub score: u32,
    /// Character positions in the title that matched, sorted. Empty when the
    /// match was in the project, branch or prompt instead.
    pub title_matches: Vec<u32>,
}

pub struct Searcher {
    matcher: Matcher,
    chars: Vec<char>,
    indices: Vec<u32>,
}

impl Default for Searcher {
    fn default() -> Self {
        Searcher::new()
    }
}

impl Searcher {
    pub fn new() -> Searcher {
        Searcher {
            matcher: Matcher::new(Config::DEFAULT),
            chars: Vec::new(),
            indices: Vec::new(),
        }
    }

    /// Best matches first. An empty query returns every session in its
    /// existing order.
    pub fn search(&mut self, query: &str, sessions: &[Session]) -> Vec<Hit> {
        let query = query.trim();
        if query.is_empty() {
            return (0..sessions.len())
                .map(|index| Hit {
                    index,
                    score: 0,
                    title_matches: Vec::new(),
                })
                .collect();
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        // Outside the title the text is long enough that scattered letters
        // match almost anything, so each word must appear intact there.
        let exact_words = query
            .split_whitespace()
            .map(|word| format!("'{}", word.trim_start_matches(['\'', '^', '!'])))
            .collect::<Vec<_>>()
            .join(" ");
        let exact = Pattern::parse(&exact_words, CaseMatching::Ignore, Normalization::Smart);

        let mut hits = Vec::new();
        let mut rest = String::new();
        for (index, session) in sessions.iter().enumerate() {
            // A match in the title outranks one elsewhere.
            self.indices.clear();
            let title = Utf32Str::new(session.display_title(), &mut self.chars);
            if let Some(score) = pattern.indices(title, &mut self.matcher, &mut self.indices) {
                let mut title_matches = self.indices.clone();
                title_matches.sort_unstable();
                title_matches.dedup();
                hits.push(Hit {
                    index,
                    score: score.saturating_mul(2),
                    title_matches,
                });
                continue;
            }

            rest.clear();
            rest.push_str(&session.title);
            rest.push(' ');
            rest.push_str(session.project());
            rest.push(' ');
            rest.push_str(session.branch.as_deref().unwrap_or(""));
            rest.push(' ');
            rest.push_str(&session.first_prompt);
            if let Some(summary) = &session.summary {
                rest.push(' ');
                rest.push_str(&summary.summary);
            }
            let haystack = Utf32Str::new(&rest, &mut self.chars);
            if exact.score(haystack, &mut self.matcher).is_some() {
                hits.push(Hit {
                    index,
                    score: 1,
                    title_matches: Vec::new(),
                });
            }
        }
        // Stable sort keeps the newest-first order among equal scores.
        hits.sort_by(|a, b| b.score.cmp(&a.score));
        hits
    }
}
