/// Convert standard markdown to Discord's markdown dialect.
///
/// Discord supports a subset of markdown with some differences:
/// - Code blocks: standard ``` works
/// - Bold: **text**
/// - Italic: *text* or _text_
/// - Strikethrough: ~~text~~
/// - Spoilers: ||text|| (Discord-specific)
/// - No HTML (unlike Telegram which uses HTML parse mode)
/// - Mentions: <@user_id>, <#channel_id>, <@&role_id>
/// - No nested formatting inside code spans
pub fn to_discord_markdown(input: &str) -> String {
    // Discord markdown is close enough to standard that most content passes through.
    // Key transforms:
    // 1. Strip any HTML tags that might have come from internal rendering
    // 2. Ensure code blocks use ``` fences (not indentation)
    // 3. Truncate to Discord's 2000 char message limit if needed

    let stripped = strip_html(input);
    stripped
}

fn strip_html(input: &str) -> String {
    // Simple tag stripper — not a full HTML parser, but handles the common cases
    // produced by internal markdown renderers (e.g. <b>, <i>, <code>, <pre>).
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut tag_buf = String::new();

    for ch in input.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag_buf.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                // Translate HTML formatting tags to Discord markdown
                match tag_buf.trim().to_lowercase().as_str() {
                    "b" | "strong" => out.push_str("**"),
                    "/b" | "/strong" => out.push_str("**"),
                    "i" | "em" => out.push('*'),
                    "/i" | "/em" => out.push('*'),
                    "code" => out.push('`'),
                    "/code" => out.push('`'),
                    "pre" => out.push_str("```\n"),
                    "/pre" => out.push_str("\n```"),
                    "br" | "br/" => out.push('\n'),
                    _ => {} // drop other tags
                }
            }
            _ if in_tag => tag_buf.push(ch),
            _ => out.push(ch),
        }
    }

    out
}

/// Split a reply into chunks that fit Discord's 2000-character message limit.
/// Tries to split at newlines to avoid breaking mid-sentence.
pub fn split_for_discord(text: &str) -> Vec<String> {
    const LIMIT: usize = 2000;

    if text.len() <= LIMIT {
        return vec![text.to_string()];
    }

    let mut chunks = vec![];
    let mut remaining = text;

    while remaining.len() > LIMIT {
        // Try to split at a newline near the limit
        let split_at = remaining[..LIMIT]
            .rfind('\n')
            .unwrap_or(LIMIT);

        let (chunk, rest) = remaining.split_at(split_at);
        if !chunk.is_empty() {
            chunks.push(chunk.to_string());
        }
        remaining = rest.trim_start_matches('\n');
    }

    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }

    chunks
}
