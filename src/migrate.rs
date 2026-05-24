//! One-time migration of configuration and state from the old `tau` paths to
//! the new `xi` paths.
//!
//! This runs at startup, before any other code reads the directories, so that
//! the rest of the application can work purely against the new paths.
//!
//! ## What is migrated
//!
//! ### XDG / platform dirs  (`tau` → `xi` app name)
//! - `<config_dir>/config.toml`
//! - `<config_dir>/tools/`
//! - `<data_dir>/auth.toml`
//! - `<data_dir>/sessions/`
//!
//! (Debug-log cache files are intentionally skipped — they are ephemeral.)
//!
//! ### Home dot-directory  (`~/.tau/` → `~/.xi/`)
//! - `AGENTS.md`
//! - `skills/`
//! - `tools/`
//!
//! ## Behaviour
//! - Only migrates a source item when the *destination* does not yet exist.
//! - Unknown files / directories under `~/.tau/` are left in place.
//! - After all known items have been moved, `~/.tau/` itself is removed only
//!   if it is empty.
//! - Errors are logged at debug level and silently ignored — a failed
//!   migration is never fatal.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the migration.  Safe to call on every startup — it is a no-op once the
/// old paths are gone or the new paths already exist.
pub fn run() {
    migrate_xdg();
    migrate_home_dot_dir();
}

// ── XDG migration ─────────────────────────────────────────────────────────────

fn migrate_xdg() {
    let (Some(old_dirs), Some(new_dirs)) = (
        ProjectDirs::from("", "", "tau"),
        ProjectDirs::from("", "", "xi"),
    ) else {
        return;
    };

    // config dir: config.toml, tools/
    move_item(
        &old_dirs.config_dir().join("config.toml"),
        &new_dirs.config_dir().join("config.toml"),
    );
    move_item(
        &old_dirs.config_dir().join("tools"),
        &new_dirs.config_dir().join("tools"),
    );

    // data dir: auth.toml, sessions/
    move_item(
        &old_dirs.data_dir().join("auth.toml"),
        &new_dirs.data_dir().join("auth.toml"),
    );
    move_item(
        &old_dirs.data_dir().join("sessions"),
        &new_dirs.data_dir().join("sessions"),
    );
}

// ── Home dot-directory migration ──────────────────────────────────────────────

fn migrate_home_dot_dir() {
    let Some(home) = std::env::var_os("HOME").filter(|s| !s.is_empty()) else {
        return;
    };
    let old_base = PathBuf::from(&home).join(".tau");
    let new_base = PathBuf::from(&home).join(".xi");

    if !old_base.exists() {
        return;
    }

    // Known items to migrate.
    for name in &["AGENTS.md", "skills", "tools"] {
        move_item(&old_base.join(name), &new_base.join(name));
    }

    // Remove the old directory if it is now empty.
    if std::fs::read_dir(&old_base).is_ok_and(|mut e| e.next().is_none()) {
        match std::fs::remove_dir(&old_base) {
            Ok(()) => log::debug!("migrate: removed empty {}", old_base.display()),
            Err(e) => {
                log::debug!(
                    "migrate: could not remove empty {}: {e}",
                    old_base.display()
                )
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Move `src` to `dst`, creating parent directories as needed.
///
/// Does nothing if `src` does not exist or `dst` already exists.
fn move_item(src: &Path, dst: &Path) {
    if !src.exists() || dst.exists() {
        return;
    }

    if dst
        .parent()
        .is_some_and(|p| std::fs::create_dir_all(p).is_err())
    {
        log::debug!("migrate: could not create parent of {}", dst.display());
        return;
    }

    match std::fs::rename(src, dst) {
        Ok(()) => {
            log::debug!("migrate: moved {} → {}", src.display(), dst.display());
        }
        Err(e) => {
            log::debug!(
                "migrate: could not move {} → {}: {e}",
                src.display(),
                dst.display()
            );
        }
    }
}
