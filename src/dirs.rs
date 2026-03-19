use std::sync::LazyLock;

use anyhow::Context;
use directories::ProjectDirs;

/// Single shared `ProjectDirs` instance for the whole application.
///
/// `None` only when the platform cannot resolve a home directory (extremely
/// rare in practice).
pub static PROJECT_DIRS: LazyLock<Option<ProjectDirs>> =
    LazyLock::new(|| ProjectDirs::from("", "", "tau"));

/// Returns a reference to the shared [`ProjectDirs`], or an error if the
/// platform failed to resolve a home directory.
pub fn project_dirs() -> anyhow::Result<&'static ProjectDirs> {
    PROJECT_DIRS
        .as_ref()
        .context("Could not resolve platform directories for tau")
}

/// Print all paths tau uses to stdout, one labelled entry per line.
/// Called by `--print-dirs`.
pub fn print_dirs() {
    let Some(dirs) = PROJECT_DIRS.as_ref() else {
        eprintln!("error: could not resolve platform directories");
        return;
    };

    let rows: &[(&str, &str, &dyn Fn() -> std::path::PathBuf)] = &[
        (
            "config",
            "config.toml  — provider, model, and general settings",
            &|| dirs.config_dir().join("config.toml"),
        ),
        (
            "auth",
            "auth.toml    — stored authentication tokens",
            &|| dirs.data_dir().join("auth.toml"),
        ),
        ("sessions", "sessions/    — conversation history", &|| {
            dirs.data_dir().join("sessions")
        }),
        (
            "logs",
            "tau-debug-*  — debug logs (enabled by TAU_DEBUG=1)",
            &|| dirs.cache_dir().to_path_buf(),
        ),
    ];

    let label_width = rows.iter().map(|(k, _, _)| k.len()).max().unwrap_or(0);

    for (label, purpose, path_fn) in rows {
        println!(
            "{:<width$}  {}  ({})",
            label,
            purpose,
            path_fn().display(),
            width = label_width,
        );
    }
}
