<img src="crates/app/icon/png/128.png" alt="Scrollback icon" width="96" height="96">

# Scrollback

Your coding-agent sessions in one place, each with a short summary of what
happened and what was left open, written by Claude Haiku through the Claude
Code command line you already use.

Scrollback reads the session history your coding agents already keep
(Claude Code, Codex, OpenCode, Cursor, GitHub Copilot, Grok and more),
indexes it, and writes a summary beside each session,
so you can find last Tuesday's work by what it was about instead of
scrolling through transcripts.

## Features

- **A small, fast native app.** Built in Rust with [GPUI](https://www.gpui.rs),
  the UI framework behind the Zed editor. It opens with your sessions already
  on screen and stays out of the way in the menu bar.
- **Fast search.** Type anywhere in the window to search titles, projects,
  summaries and every message in every session, with results as you type.
- **Summaries by default.** Once a session goes quiet, Scrollback writes a
  title, a short summary, how it ended and anything left to pick up. Sessions
  you are still working in are summarised as you go.
- **Transcripts grouped by step.** Open a session to read it as the steps the
  summary describes, each folding open to the turns behind it.
- **One-step resume.** Copy the command that reopens a session in the tool it
  came from.
- **Find and export.** Find text inside a session (⌘F), or save it as
  Markdown (⇧⌘E, or `scrollback export`).
- **Open at login.** Start in the menu bar when you log in.
- **Activity.** Your week at a glance: time in use each day, which tools you
  used, and the open ends still waiting.
- **Menu bar popover.** Recent summaries and the week's activity, a click
  away, without opening the window.
- **Read-only and local.** Scrollback never writes to your tools' files, and
  its index stays on your computer.

## Sources

| Source | Read from |
| --- | --- |
| Claude Code, and the Claude app's coding sessions | `~/.claude/projects` |
| Codex, and the ChatGPT app's Codex sessions | `~/.codex` |
| OpenCode | `~/.local/share/opencode` |
| pi | `~/.pi/agent/sessions` |
| Cursor | Cursor's own data folder |
| Antigravity | `~/.gemini/antigravity` and `~/.gemini/antigravity-ide` |
| fx | `~/.fx/sessions` |
| Grok Build | `~/.grok/sessions` |
| Hermes Agent | `~/.hermes` (`%LOCALAPPDATA%\hermes` on Windows) |
| GitHub Copilot CLI | `~/.copilot/session-state` |
| GitHub Copilot in VS Code | VS Code's user folder, `chatSessions` |
| T3 Code, Paseo | label the sessions they started in the tools above |

Each folder can be changed in Settings → Sources, and any source can be
hidden there. Folders also follow each tool's own setting where it has one,
such as `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `FX_HOME`, `GROK_HOME`,
`HERMES_HOME` or `COPILOT_HOME`.

A few notes:

- **Antigravity** encrypts its conversations. Scrollback shows each one from
  what Antigravity leaves readable: its title, folder and times, the agent's
  latest message, and the task list, plan and walkthrough it wrote.
- **Orca** runs other agents' command-line tools and keeps no conversations
  of its own, so its sessions appear under the tool that ran them.
- fx, Grok, Hermes and Copilot are read from their documented formats
  and have had less real-world use than the others.

## Getting started

Scrollback is built from source for now. You need
[Rust](https://rustup.rs) (stable) and, on a Mac, the Xcode command line tools
(`xcode-select --install`).

### macOS

```sh
git clone https://github.com/frankievalentine/Scrollback.git scrollback
cd scrollback
scripts/bundle.sh --install
```

This builds `Scrollback.app`, installs it in `/Applications` and signs it for
this Mac. Open it from Launchpad or Spotlight.

### Linux

Install the libraries GPUI builds against (Debian and Ubuntu shown):

```sh
sudo apt install pkg-config cmake clang libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev \
  libssl-dev libzstd-dev libvulkan-dev libasound2-dev
```

Then build a package and install it:

```sh
scripts/package-linux.sh
sudo apt install ./target/package/scrollback_*.deb
```

Or unpack `target/package/scrollback-*.tar.gz` and run `./install.sh` to
install into `~/.local` without root.

### Windows

```powershell
pwsh scripts/package-windows.ps1
```

This leaves a zip in `target\package`, and an installer too if
[Inno Setup](https://jrsoftware.org/isinfo.php) is installed.

Linux and Windows builds are newer than the Mac one and have had less use.

## Summaries

Summaries are written by the [Claude Code](https://claude.com/claude-code)
command-line tool, using the login it already has, so there is no API key to
set up. If `claude` is installed and signed in, summaries start on their own;
if not, everything else works and sessions show their first prompt instead.

What is sent for each session: your prompts, the agent's replies, and the
names and targets of the tools it used. Tool output and file contents are not
sent. Summaries count towards your Claude plan; Haiku is used by default, and
Settings → Summaries can change the model or turn automatic summaries off.

## Command line

The same index is available from a terminal:

```sh
scrollback list                  # recent sessions
scrollback search token refresh  # fuzzy search
scrollback show runyard redesign # a session's summary
scrollback summarize -n 10       # summarise the ten newest without one
scrollback export runyard -o runyard.md  # a session as Markdown
scrollback --help                # everything else
```

On a Mac the command is built at `target/release/scrollback`; the Linux
packages install it on your path.

## Where things are kept

| | macOS | Linux | Windows |
| --- | --- | --- | --- |
| Index and settings | `~/Library/Application Support/Scrollback` | `~/.local/share/scrollback` | `%LOCALAPPDATA%\Scrollback` |

The Linux folder is lower case, like the other tools in `~/.local/share`. A
folder left from an older version under a different spelling is renamed the
first time Scrollback starts.

Deleting that folder resets Scrollback; it rebuilds the index on its next
start and writes the summaries again.

## Development

```sh
cargo run --release -p scrollback-app   # the app
cargo test                              # the tests
scripts/check-platforms.sh              # Linux tests and a Windows build, in Docker
scripts/linux-preview.sh                # a picture of the app running on Linux
```

The code is in three crates: `crates/core` reads the sources, keeps the index
and writes summaries; `crates/app` is the GPUI app; `crates/cli` is the
command line.

## License

MIT. See [LICENSE](LICENSE).
