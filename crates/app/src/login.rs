//! Starting Scrollback when you log in. Each system has its own place for
//! this, and each is a plain file or setting the user can see and remove:
//!
//! - macOS: a launch agent, `~/Library/LaunchAgents/local.scrollback.plist`
//! - Linux: an autostart entry, `~/.config/autostart/scrollback.desktop`
//! - Windows: a value named Scrollback under the current user's `Run` key
//!
//! Started this way, the app is given `--at-login` and starts in the menu
//! bar without opening its window, when the menu bar icon is on.

use std::path::PathBuf;

/// The argument a login start is given.
pub const AT_LOGIN: &str = "--at-login";

/// Whether Scrollback is set to start at login, read from the system each
/// time so a change made outside the app shows.
pub fn enabled() -> bool {
    platform::enabled()
}

/// Start, or stop starting, at login. Returns why it could not be done.
pub fn set(on: bool) -> Result<(), String> {
    let program = program().ok_or("could not tell where Scrollback is installed")?;
    platform::set(on, &program).map_err(|err| err.to_string())
}

/// What to start: the running program. On a Mac that is the executable
/// inside the app bundle, which is started directly.
fn program() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|path| path.canonicalize().ok())
}

#[cfg(target_os = "macos")]
mod platform {
    use super::AT_LOGIN;
    use std::path::{Path, PathBuf};

    const LABEL: &str = "local.scrollback";

    fn agent() -> PathBuf {
        scrollback_core::home().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
    }

    pub fn enabled() -> bool {
        agent().is_file()
    }

    pub fn set(on: bool, program: &Path) -> std::io::Result<()> {
        let path = agent();
        if !on {
            return match std::fs::remove_file(&path) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            };
        }
        let escape = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>{AT_LOGIN}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
            escape(&program.to_string_lossy())
        );
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        std::fs::write(&path, plist)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::AT_LOGIN;
    use std::path::{Path, PathBuf};

    fn entry() -> PathBuf {
        scrollback_core::env_dir("XDG_CONFIG_HOME")
            .unwrap_or_else(|| scrollback_core::home().join(".config"))
            .join("autostart")
            .join("scrollback.desktop")
    }

    pub fn enabled() -> bool {
        entry().is_file()
    }

    pub fn set(on: bool, program: &Path) -> std::io::Result<()> {
        let path = entry();
        if !on {
            return match std::fs::remove_file(&path) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            };
        }
        // A desktop entry's Exec line quotes arguments with double quotes.
        let quoted = format!("\"{}\"", program.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\""));
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=Scrollback\nExec={quoted} {AT_LOGIN}\n\
             Icon=scrollback\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        );
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        std::fs::write(&path, entry)
    }
}

#[cfg(windows)]
mod platform {
    use super::AT_LOGIN;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::Command;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Scrollback";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn reg(args: &[&str]) -> std::io::Result<bool> {
        let status = Command::new("reg")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        Ok(status.success())
    }

    pub fn enabled() -> bool {
        reg(&["query", KEY, "/v", NAME]).unwrap_or(false)
    }

    pub fn set(on: bool, program: &Path) -> std::io::Result<()> {
        if !on {
            reg(&["delete", KEY, "/v", NAME, "/f"])?;
            return Ok(());
        }
        let value = format!("\"{}\" {AT_LOGIN}", program.to_string_lossy());
        if reg(&["add", KEY, "/v", NAME, "/t", "REG_SZ", "/d", &value, "/f"])? {
            Ok(())
        } else {
            Err(std::io::Error::other("the registry refused the change"))
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_login_item_is_written_and_removed() {
        let home = std::env::temp_dir().join(format!("scrollback-login-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        std::env::remove_var("XDG_CONFIG_HOME");
        let program = std::path::Path::new("/Applications/Scrollback.app/Contents/MacOS/scrollback");
        assert!(!enabled());
        platform::set(true, program).unwrap();
        assert!(enabled());
        let written = std::fs::read_dir(&home)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        assert!(!written.is_empty());
        platform::set(false, program).unwrap();
        assert!(!enabled());
        std::fs::remove_dir_all(&home).ok();
    }
}
