use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

// ── Skill types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The markdown body used as the skill's system prompt.
    pub system_prompt: String,
    pub source: PathBuf,
}

// ── Frontmatter parsing ───────────────────────────────────────────────────────

/// Splits a markdown file into (frontmatter YAML, body).
/// Returns ("", full_content) when no `---` delimiters are found.
fn split_frontmatter(content: &str) -> (&str, &str) {
    let content = content.trim_start_matches('\n');
    if !content.starts_with("---") {
        return ("", content);
    }
    let after_open = &content[3..];
    // Allow `---` or `---\n`; find the closing delimiter.
    let rest = after_open.trim_start_matches('\n');
    if let Some(close) = rest.find("\n---") {
        let fm = &rest[..close];
        let body = &rest[close + 4..]; // skip "\n---"
        let body = body.trim_start_matches('\n');
        (fm, body)
    } else {
        ("", content)
    }
}

// ── Loading ───────────────────────────────────────────────────────────────────

fn load_skill_from_file(path: &Path) -> Result<Skill> {
    let content =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let (fm_str, body) = split_frontmatter(&content);

    let fm: SkillFrontmatter = if fm_str.is_empty() {
        SkillFrontmatter::default()
    } else {
        serde_yaml::from_str(fm_str)
            .with_context(|| format!("parsing frontmatter in {}", path.display()))?
    };

    // Fall back to the stem of the filename when `name` is absent.
    let name = fm.name.unwrap_or_else(|| {
        path.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });

    let description = fm.description.unwrap_or_default();

    Ok(Skill {
        name,
        description,
        system_prompt: body.to_string(),
        source: path.to_path_buf(),
    })
}

fn load_from_dir(dir: &Path) -> Result<Vec<Skill>> {
    let mut skills = Vec::new();

    let mut entries: Vec<_> = match fs::read_dir(dir) {
        Ok(iter) => iter.filter_map(|e| e.ok()).collect(),
        Err(_) => return Ok(skills),
    };
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let skill_path = if path.is_file() && path.extension().map_or(false, |e| e == "md") {
            path.clone()
        } else if path.is_dir() {
            let candidate = path.join("skill.md");
            if candidate.exists() {
                candidate
            } else {
                continue;
            }
        } else {
            continue;
        };

        match load_skill_from_file(&skill_path) {
            Ok(skill) => skills.push(skill),
            Err(e) => eprintln!("warning: skipping invalid skill definition: {}", e),
        }
    }

    Ok(skills)
}

// ── Registry ──────────────────────────────────────────────────────────────────

pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Loads skills from global (~/.cooper/skills) and project (.agents/skills) directories.
    /// Project skills override globals of the same name.
    pub fn load() -> Result<Self> {
        let mut skills: Vec<Skill> = Vec::new();

        if let Some(home) = dirs::home_dir() {
            let global_dir = home.join(".cooper").join("skills");
            skills.extend(load_from_dir(&global_dir)?);
        }

        let project_dir = PathBuf::from(".agents/skills");
        for skill in load_from_dir(&project_dir)? {
            skills.retain(|s| s.name != skill.name);
            skills.push(skill);
        }

        Ok(Self { skills })
    }

    pub fn all(&self) -> &[Skill] {
        &self.skills
    }

    pub fn find(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }
}
