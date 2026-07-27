//! Output sanitation: models decorate output no matter what the prompt says.
//!
//! The system prompt (see [`crate::prompt`]) instructs the model to return
//! only the transformed text. Small instruct models comply *most* of the
//! time; the rest of the time they wrap output in code fences, prefix it with
//! "Here is the tightened text:", quote it, or leak a `<think>` block. Every
//! rule below exists because some model family actually does the thing, so
//! sanitation is defence, not pedantry.
//!
//! Order matters: thinking blocks first (they can contain fences), then
//! preamble lines (so a stripped pleasantry exposes a whole-output fence),
//! then fences, then quotes, then whitespace. Each pass is
//! conservative: when in doubt, leave text alone and let the guardrails
//! (which compare against the original) catch what slips through.

/// Strip model decoration from `raw`, returning what should be the pure
/// transformed text. `instruction` is used to recognise echo prefixes.
pub fn sanitize(raw: &str, instruction: &str) -> String {
    let mut text = raw.trim().to_string();

    text = strip_thinking_blocks(&text);
    // Preamble before fence: "Sure!\n```\n...\n```" needs the pleasantry
    // gone before the fence looks like it wraps the whole output.
    text = strip_preamble_lines(&text, instruction);
    text = strip_code_fence(&text);
    text = strip_wrapping_quotes(&text);

    text.trim().to_string()
}

/// Qwen3-class models emit `<think>...</think>` even with thinking nominally
/// disabled; anything inside is reasoning, never output.
fn strip_thinking_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(end) => rest = &rest[start + end + "</think>".len()..],
            None => {
                // Unclosed block: everything after `<think>` is reasoning.
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Unwrap a single outer ``` fence (with or without a language tag). Only
/// when the fence encloses the *whole* output: a fence in the middle is
/// legitimate content (the user may be editing markdown).
fn strip_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    let Some(first_newline) = trimmed.find('\n') else {
        // "```" alone or "```text" with no body.
        return String::new();
    };
    let body = &trimmed[first_newline + 1..];
    let Some(close) = body.rfind("```") else {
        // Opening fence, no close: model ran out of tokens. Take the body.
        return body.trim().to_string();
    };
    let inner = &body[..close];
    let after = body[close + 3..].trim();
    // Text after the closing fence means the fence wrapped only part of the
    // output; safer to keep everything except the fence markers themselves.
    if after.is_empty() {
        inner.trim().to_string()
    } else {
        format!("{} {}", inner.trim(), after)
    }
}

/// Drop leading lines that are commentary about the transformation rather
/// than the transformation ("Here is the tightened text:", "Sure!", an echo
/// of the instruction). Only *leading* lines are candidates, and only when
/// they end with a colon or are a known pleasantry, because a legitimate
/// first line of transformed text must survive.
fn strip_preamble_lines(text: &str, instruction: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    while let Some(first) = lines.first() {
        let line = first.trim();
        if line.is_empty() {
            lines.remove(0);
            continue;
        }
        if is_preamble_line(line, instruction) {
            lines.remove(0);
            continue;
        }
        break;
    }
    lines.join("\n").trim().to_string()
}

fn is_preamble_line(line: &str, instruction: &str) -> bool {
    let lower = line.to_lowercase();
    // "Here is the ...:", "Here's your ...:", "Below is ...:" etc. The
    // trailing colon is the giveaway that the line announces rather than is.
    let announces = lower.ends_with(':')
        && [
            "here is",
            "here's",
            "here are",
            "below is",
            "the following",
            "sure",
            "certainly",
            "okay",
            "of course",
        ]
        .iter()
        .any(|p| lower.starts_with(p));
    // Bare pleasantries with no content.
    let pleasantry = matches!(
        lower.trim_end_matches(['!', '.']),
        "sure" | "certainly" | "of course" | "okay" | "no problem"
    );
    // The model sometimes echoes the instruction back as a header line.
    // Require a non-trivial instruction so a 1-2 char substring cannot
    // match by accident inside an unrelated first line.
    let echoes_instruction = instruction.len() >= 4
        && lower.contains(&instruction.to_lowercase())
        && line.len() < instruction.len() + 24;
    announces || pleasantry || echoes_instruction
}

/// Unwrap quotes only when they enclose the entire output *and* the original
/// convention is clearly the model's (matching pair at both ends). A quoted
/// sentence the user actually wants stays quoted because interior quotes
/// will not match this shape... unless the whole text is one quotation, an
/// accepted false positive the preview makes visible.
fn strip_wrapping_quotes(text: &str) -> String {
    let t = text.trim();
    for (open, close) in [
        ('"', '"'),
        ('\u{201c}', '\u{201d}'),
        ('\u{2018}', '\u{2019}'),
    ] {
        if t.len() >= 2 && t.starts_with(open) && t.ends_with(close) {
            let inner = &t[open.len_utf8()..t.len() - close.len_utf8()];
            // Interior matching quote chars imply structure we should not touch.
            if !inner.contains(open) && !inner.contains(close) {
                return inner.trim().to_string();
            }
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_output_passes_through() {
        assert_eq!(
            sanitize("The deploy must happen today.", "tighten"),
            "The deploy must happen today."
        );
    }

    #[test]
    fn strips_full_code_fence() {
        assert_eq!(sanitize("```\nHello world.\n```", "x"), "Hello world.");
    }

    #[test]
    fn strips_fence_with_language_tag() {
        assert_eq!(sanitize("```text\nHello world.\n```", "x"), "Hello world.");
    }

    #[test]
    fn keeps_interior_fence() {
        let s = "Intro line.\n```\ncode\n```\nOutro.";
        assert_eq!(sanitize(s, "x"), s);
    }

    #[test]
    fn unclosed_fence_takes_body() {
        assert_eq!(sanitize("```\nHalf done", "x"), "Half done");
    }

    #[test]
    fn strips_here_is_prefix() {
        assert_eq!(
            sanitize(
                "Here is the tightened text:\nDeploy today.",
                "tighten this up"
            ),
            "Deploy today."
        );
    }

    #[test]
    fn strips_pleasantry_then_fence() {
        assert_eq!(
            sanitize("Sure!\n```\nDeploy today.\n```", "tighten"),
            "Deploy today."
        );
    }

    #[test]
    fn strips_instruction_echo_header() {
        assert_eq!(
            sanitize("Tighten this up:\nDeploy today.", "tighten this up"),
            "Deploy today."
        );
    }

    #[test]
    fn keeps_first_line_of_real_content() {
        let s = "Deploy today.\nThen celebrate.";
        assert_eq!(sanitize(s, "tighten"), s);
    }

    #[test]
    fn strips_thinking_block() {
        assert_eq!(
            sanitize("<think>user wants it shorter</think>Deploy today.", "x"),
            "Deploy today."
        );
    }

    #[test]
    fn unclosed_thinking_block_yields_nothing() {
        assert_eq!(sanitize("<think>hmm, let me consider", "x"), "");
    }

    #[test]
    fn strips_wrapping_double_quotes() {
        assert_eq!(sanitize("\"Deploy today.\"", "x"), "Deploy today.");
    }

    #[test]
    fn strips_smart_quotes() {
        assert_eq!(
            sanitize("\u{201c}Deploy today.\u{201d}", "x"),
            "Deploy today."
        );
    }

    #[test]
    fn keeps_interior_quotes() {
        let s = "She said \"deploy\" and left.";
        assert_eq!(sanitize(s, "x"), s);
    }

    #[test]
    fn empty_fence_yields_empty() {
        assert_eq!(sanitize("```\n```", "x"), "");
    }

    #[test]
    fn combined_worst_case() {
        let raw =
            "<think>ok</think>\nCertainly!\nHere's the result:\n```text\n\"Deploy today.\"\n```";
        assert_eq!(sanitize(raw, "tighten this up"), "Deploy today.");
    }
}
