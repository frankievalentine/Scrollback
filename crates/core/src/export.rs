//! A session written out as Markdown: its title and details, the summary
//! when there is one, and the whole conversation, for keeping or sharing.

use crate::model::Session;
use crate::transcript::{self, Block};
use anyhow::Result;
use chrono::{Local, TimeZone};

/// Read the session from its source and write it out.
pub fn markdown(session: &Session) -> Result<String> {
    let blocks = transcript::blocks(transcript::events(session)?);
    Ok(render(session, &blocks))
}

/// A file name for the export: the title, made safe for any system.
pub fn file_name(session: &Session) -> String {
    let mut name: String = session
        .display_title()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() {
        name = format!("session {}", session.id);
    }
    name.truncate(80);
    format!("{}.md", name.trim_end())
}

pub fn render(session: &Session, blocks: &[Block]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", session.display_title()));
    let when = |ms: i64| {
        Local
            .timestamp_millis_opt(ms)
            .single()
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    };
    let mut facts = vec![
        format!("- **Source:** {}", session.origin.name()),
        format!("- **Project:** {}", session.project()),
        format!("- **Folder:** `{}`", session.cwd),
    ];
    if let Some(branch) = session.branch.as_deref().filter(|b| !b.is_empty() && *b != "HEAD") {
        facts.push(format!("- **Branch:** {branch}"));
    }
    facts.push(format!("- **Started:** {}", when(session.started_ms)));
    facts.push(format!("- **Last used:** {}", when(session.updated_ms)));
    facts.push(format!("- **Session:** `{}`", session.key()));
    out.push_str(&facts.join("\n"));
    out.push_str("\n\n");

    if let Some(summary) = &session.summary {
        out.push_str("## Summary\n\n");
        out.push_str(&summary.summary);
        out.push_str(&format!("\n\n**Outcome:** {}\n", summary.outcome.label()));
        let list = |out: &mut String, title: &str, lines: &[String], numbered: bool| {
            if lines.is_empty() {
                return;
            }
            out.push_str(&format!("\n### {title}\n\n"));
            for (i, line) in lines.iter().enumerate() {
                match numbered {
                    true => out.push_str(&format!("{}. {line}\n", i + 1)),
                    false => out.push_str(&format!("- {line}\n")),
                }
            }
        };
        list(&mut out, "What happened", &summary.happened, true);
        list(&mut out, "Open ends", &summary.open_ends, false);
        list(&mut out, "Friction", &summary.friction, false);
        out.push('\n');
    }

    out.push_str("## Conversation\n");
    for block in blocks {
        match block {
            Block::Prompt(text) => out.push_str(&format!("\n### You\n\n{}\n", text.trim())),
            Block::Reply(text) => out.push_str(&format!("\n### Agent\n\n{}\n", text.trim())),
            Block::Tools(calls) => {
                out.push('\n');
                for (name, target) in calls {
                    match target {
                        Some(target) => out.push_str(&format!("- `{name}` {target}\n")),
                        None => out.push_str(&format!("- `{name}`\n")),
                    }
                }
            }
            Block::Note(text) => out.push_str(&format!("\n> {}\n", text.trim())),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Harness;

    #[test]
    fn a_session_reads_as_markdown() {
        let mut session = Session::new(Harness::ClaudeCode, "abc", "a.jsonl");
        session.title = "Fix the login: redirect/loop".into();
        session.cwd = "/p/app".into();
        let blocks = vec![
            Block::Prompt("Why does login loop?".into()),
            Block::Tools(vec![("Read".into(), Some("src/auth.rs".into())), ("Bash".into(), None)]),
            Block::Reply("The cookie was never set.".into()),
        ];
        let text = render(&session, &blocks);
        assert!(text.starts_with("# Fix the login: redirect/loop\n"));
        assert!(text.contains("### You\n\nWhy does login loop?"));
        assert!(text.contains("- `Read` src/auth.rs\n- `Bash`\n"));
        assert!(text.contains("### Agent\n\nThe cookie was never set."));
        assert_eq!(file_name(&session), "Fix the login redirect loop.md");
    }
}
