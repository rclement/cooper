use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
}

/// Splits a markdown string into `(frontmatter_yaml, body)`.
/// Returns `("", full_content)` when no `---` delimiters are found.
pub fn split_frontmatter(content: &str) -> (&str, &str) {
    let content = content.trim_start_matches('\n');
    if !content.starts_with("---") {
        return ("", content);
    }
    let after_open = &content[3..];
    let rest = after_open.trim_start_matches('\n');
    if let Some(close) = rest.find("\n---") {
        let fm = &rest[..close];
        let body = rest[close + 4..].trim_start_matches('\n');
        (fm, body)
    } else {
        ("", content)
    }
}

/// Parse a `Skill` from its markdown content.
/// `name_hint` is used as the skill name when the frontmatter omits it.
pub fn parse_skill(content: &str, name_hint: &str) -> Result<Skill> {
    let (fm_str, body) = split_frontmatter(content);
    let fm: SkillFrontmatter = if fm_str.is_empty() {
        SkillFrontmatter::default()
    } else {
        serde_yaml::from_str(fm_str).context("parsing skill frontmatter")?
    };
    Ok(Skill {
        name: fm.name.unwrap_or_else(|| name_hint.to_string()),
        description: fm.description.unwrap_or_default(),
        system_prompt: body.to_string(),
    })
}

pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    pub fn new(skills: Vec<Skill>) -> Self {
        Self { skills }
    }

    pub fn empty() -> Self {
        Self { skills: vec![] }
    }

    pub fn all(&self) -> &[Skill] {
        &self.skills
    }

    pub fn find(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── split_frontmatter ─────────────────────────────────────────────────────

    #[test]
    fn split_no_frontmatter() {
        let (fm, body) = split_frontmatter("just body");
        assert_eq!(fm, "");
        assert_eq!(body, "just body");
    }

    #[test]
    fn split_with_frontmatter() {
        let content = "---\nname: my-skill\n---\nbody content";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.contains("name: my-skill"));
        assert_eq!(body, "body content");
    }

    #[test]
    fn split_leading_newlines_stripped() {
        let content = "\n---\nname: x\n---\nbody";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.contains("name: x"));
        assert_eq!(body, "body");
    }

    #[test]
    fn split_no_closing_delimiter_returns_empty_fm() {
        let (fm, _) = split_frontmatter("---\nname: x\nbody");
        assert_eq!(fm, "");
    }

    // ── parse_skill ───────────────────────────────────────────────────────────

    #[test]
    fn parse_with_frontmatter() {
        let content = "---\nname: code-review\ndescription: Reviews code\n---\nDo a thorough review.";
        let skill = parse_skill(content, "fallback").unwrap();
        assert_eq!(skill.name, "code-review");
        assert_eq!(skill.description, "Reviews code");
        assert!(skill.system_prompt.contains("thorough review"));
    }

    #[test]
    fn parse_without_frontmatter_uses_hint() {
        let skill = parse_skill("Refactor the code.", "refactor").unwrap();
        assert_eq!(skill.name, "refactor");
        assert_eq!(skill.description, "");
        assert!(skill.system_prompt.contains("Refactor"));
    }

    #[test]
    fn parse_invalid_frontmatter_errors() {
        let result = parse_skill("---\n- list item\n---\nbody", "x");
        assert!(result.is_err());
    }

    // ── SkillRegistry ─────────────────────────────────────────────────────────

    #[test]
    fn registry_empty() {
        let reg = SkillRegistry::empty();
        assert!(reg.all().is_empty());
        assert!(reg.find("anything").is_none());
    }

    #[test]
    fn registry_find() {
        let reg = SkillRegistry::new(vec![
            parse_skill("---\nname: foo\n---\nFoo body", "foo").unwrap(),
        ]);
        assert!(reg.find("foo").is_some());
        assert!(reg.find("bar").is_none());
    }

    #[test]
    fn registry_all() {
        let reg = SkillRegistry::new(vec![
            parse_skill("A", "a").unwrap(),
            parse_skill("B", "b").unwrap(),
        ]);
        assert_eq!(reg.all().len(), 2);
    }
}
