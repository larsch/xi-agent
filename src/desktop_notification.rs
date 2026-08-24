//! Best-effort desktop notifications for agent-loop halt points.

const MAX_CONTENT_CHARS: usize = 200;

fn notification_title(cwd: &std::path::Path) -> String {
    let project = cwd
        .file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned());
    format!("xi in {project}")
}

fn truncate_content(content: &str) -> String {
    if content.chars().count() <= MAX_CONTENT_CHARS {
        return content.to_string();
    }

    let mut truncated: String = content.chars().take(MAX_CONTENT_CHARS - 1).collect();
    truncated.push('…');
    truncated
}

/// Show a desktop notification without blocking the UI or failing the agent run.
pub(crate) fn notify_agent_loop_halt(content: &str) {
    #[cfg(not(test))]
    {
        let cwd = std::env::current_dir().unwrap_or_default();
        let title = notification_title(&cwd);
        let body = truncate_content(content);
        std::thread::spawn(move || {
            if let Err(error) = notify_rust::Notification::new()
                .summary(&title)
                .body(&body)
                .show()
            {
                log::debug!("failed to show desktop notification: {error}");
            }
        });
    }

    #[cfg(test)]
    let _ = content;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_uses_last_directory_name() {
        assert_eq!(
            notification_title(std::path::Path::new("/work/projects/xi-agent")),
            "xi in xi-agent"
        );
    }

    #[test]
    fn short_content_is_unchanged() {
        assert_eq!(truncate_content("What should I do?"), "What should I do?");
    }

    #[test]
    fn long_content_is_limited_to_200_characters() {
        let content = "é".repeat(201);
        let truncated = truncate_content(&content);
        assert_eq!(truncated.chars().count(), MAX_CONTENT_CHARS);
        assert!(truncated.ends_with('…'));
    }
}
