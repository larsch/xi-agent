use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

/// Background colour for the info bar.
const INFO_BG: Color = Color::Rgb(20, 20, 30);

/// Build the single info-bar `Line` showing provider / model / context window.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_info_line<'a>(
    provider: &str,
    model: &str,
    thinking: Option<&str>,
    context_window: Option<usize>,
    used_tokens: Option<usize>,
    cached_tokens: Option<usize>,
    cache_miss_warning: bool,
    width: usize,
) -> Line<'a> {
    let sep_style = Style::default().fg(Color::Rgb(60, 60, 80)).bg(INFO_BG);
    let key_style = Style::default().fg(Color::Rgb(100, 100, 130)).bg(INFO_BG);
    let val_style = Style::default().fg(Color::Rgb(180, 200, 255)).bg(INFO_BG);
    let fill_style = Style::default().bg(INFO_BG);
    let hint_style = Style::default().fg(Color::Rgb(60, 60, 80)).bg(INFO_BG);

    let hint = "Ctrl+I";
    let context_value = format_context_value(
        context_window,
        used_tokens,
        cached_tokens,
        cache_miss_warning,
    );
    let mut content_spans: Vec<Span<'a>> = vec![
        Span::styled(" ", fill_style),
        Span::styled("provider", key_style),
        Span::styled(" ", fill_style),
        Span::styled(provider.to_string(), val_style),
        Span::styled("  │  ", sep_style),
        Span::styled("model", key_style),
        Span::styled(" ", fill_style),
        Span::styled(model.to_string(), val_style),
    ];

    if let Some(thinking) = thinking {
        content_spans.push(Span::styled("  │  ", sep_style));
        content_spans.push(Span::styled("thinking", key_style));
        content_spans.push(Span::styled(" ", fill_style));
        content_spans.push(Span::styled(thinking.to_string(), val_style));
    }

    content_spans.push(Span::styled("  │  ", sep_style));
    content_spans.push(Span::styled("context", key_style));
    content_spans.push(Span::styled(" ", fill_style));
    content_spans.push(Span::styled(context_value.clone(), val_style));

    let mut used: usize =
        1 + "provider".len() + 1 + provider.len() + 5 + "model".len() + 1 + model.len();

    if let Some(thinking) = thinking {
        used += 5 + "thinking".len() + 1 + thinking.len();
    }

    used += 5 + "context".len() + 1 + context_value.len();

    let hint_len = hint.len() + 1;
    let fill_len = width.saturating_sub(used + hint_len);

    let mut spans = content_spans;
    spans.push(Span::styled(" ".repeat(fill_len), fill_style));
    spans.push(Span::styled(hint.to_string(), hint_style));
    spans.push(Span::styled(" ", fill_style));

    Line::from(spans)
}

pub(super) fn format_context_value(
    context_window: Option<usize>,
    used_tokens: Option<usize>,
    cached_tokens: Option<usize>,
    cache_miss_warning: bool,
) -> String {
    let cached_suffix = match (cached_tokens, cache_miss_warning) {
        // Unexpected cache miss: previous turn should have populated the
        // cache but this turn got zero cached tokens.
        (_, true) => " ⚠️".to_string(),
        // Normal cache hit: show the number of cached tokens with a ⚡.
        (Some(n), _) if n > 0 => format!(" [{}⚡]", format_context_size(n)),
        // No cache info or no miss — no suffix.
        _ => String::new(),
    };
    match context_window {
        Some(max) => {
            let max_fmt = format_context_size(max);
            if let Some(used) = used_tokens {
                let pct = ((used.saturating_mul(100)) / max.max(1)).min(999);
                format!(
                    "{} / {} ({}%){}",
                    format_context_size(used),
                    max_fmt,
                    pct,
                    cached_suffix
                )
            } else {
                format!("{}{}", max_fmt, cached_suffix)
            }
        }
        None => format!("unknown{}", cached_suffix),
    }
}

fn format_context_size(n: usize) -> String {
    if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}
