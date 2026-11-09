//! Copy-paste recipes. The source of truth is docs/examples.md, compiled in
//! so the CLI cannot drift from the file in the repo.

pub const EXAMPLES_MD: &str = include_str!("../../../docs/examples.md");
pub const EXAMPLES_URL: &str = "https://gledach.de/leash/examples/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeaturedRecipe {
    pub title: String,
    pub what: String,
    pub code: String,
}

/// Featured recipes for the window (first N `###` sections that have a command).
pub fn featured(n: usize) -> Vec<FeaturedRecipe> {
    parse_recipes()
        .into_iter()
        .take(n)
        .collect()
}

pub fn parse_recipes() -> Vec<FeaturedRecipe> {
    let mut out = Vec::new();
    let mut title: Option<String> = None;
    let mut what = String::new();
    let mut in_code = false;
    let mut code = String::new();
    for line in EXAMPLES_MD.lines() {
        if line.starts_with("```") {
            if in_code {
                if let Some(t) = title.take() {
                    let cmd = first_command(&code);
                    if !cmd.is_empty() {
                        out.push(FeaturedRecipe {
                            title: t,
                            what: what.trim().to_string(),
                            code: cmd,
                        });
                    }
                }
                code.clear();
                what.clear();
                in_code = false;
            } else {
                in_code = true;
            }
            continue;
        }
        if in_code {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        if let Some(rest) = line.strip_prefix("### ") {
            title = Some(rest.trim().to_string());
            what.clear();
            continue;
        }
        if title.is_some() && !line.starts_with('#') && !line.trim().is_empty() {
            if !what.is_empty() {
                what.push(' ');
            }
            what.push_str(line.trim());
        }
    }
    out
}

fn first_command(block: &str) -> String {
    for line in block.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        return t.to_string();
    }
    String::new()
}

pub fn render_cli() -> String {
    let mut out = EXAMPLES_MD.trim().to_string();
    out.push_str("\n\nMore, with copy buttons: ");
    out.push_str(EXAMPLES_URL);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any_recipe_line_contains(needle: &str) -> bool {
        let mut in_code = false;
        for line in EXAMPLES_MD.lines() {
            if line.starts_with("```") {
                in_code = !in_code;
                continue;
            }
            if in_code && line.contains(needle) && !line.trim().starts_with('#') {
                return true;
            }
        }
        false
    }

    #[test]
    fn source_contains_the_main_recipes() {
        for needle in [
            "leash run --path . -- claude",
            "leash run --path . -- claude --dangerously-skip-permissions",
            "leash run --path . -- codex",
            "leash run --path . -- codex --yolo",
            "leash run --path . -- codex --sandbox workspace-write --ask-for-approval never",
            "leash run --path . -- gemini --approval-mode=yolo",
            "leash run --path . -- aider --yes-always",
            "leash start --path . --attach cursor",
            "leash start --path . --attach vscode",
            "leash start --path . --attach windsurf",
            "leash ps",
            "leash run --path . -- pnpm install",
            "leash doctor",
        ] {
            assert!(
                EXAMPLES_MD.contains(needle),
                "docs/examples.md should contain: {needle}"
            );
        }
        assert!(
            !any_recipe_line_contains("--full-auto"),
            "do not offer the removed Codex --full-auto flag as a command"
        );
        assert!(
            EXAMPLES_MD.contains("`--full-auto` was removed"),
            "explain that Codex --full-auto was removed"
        );
    }

    #[test]
    fn featured_has_titles_and_commands() {
        let feat = featured(3);
        assert_eq!(feat.len(), 3);
        assert!(feat[0].title.to_ascii_lowercase().contains("claude"));
        assert!(feat[0].code.contains("leash run"));
        assert!(!feat[0].what.is_empty());
        assert!(parse_recipes().len() >= 8);
    }
}
