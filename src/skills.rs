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

/// Test-only re-export of the private `load_from_dir` helper.
#[cfg(test)]
pub(crate) fn load_from_dir_pub(dir: &Path) -> Result<Vec<Skill>> {
    load_from_dir(dir)
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
    pub(crate) skills: Vec<Skill>,
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

    /// Like `load()` but retains only the skills named in `allowed`.
    /// `None` means all skills are allowed; `Some(&[])` means none.
    pub fn load_filtered(allowed: Option<&[String]>) -> Result<Self> {
        let mut registry = Self::load()?;
        if let Some(names) = allowed {
            registry
                .skills
                .retain(|s| names.iter().any(|n| n == &s.name));
        }
        Ok(registry)
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
    use tempfile::TempDir;

    fn write_skill(dir: &std::path::Path, filename: &str, content: &str) {
        std::fs::write(dir.join(filename), content).unwrap();
    }

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
        // Has opening --- but no closing ---
        let content = "---\nname: x\nbody";
        let (fm, _body) = split_frontmatter(content);
        assert_eq!(fm, "");
    }

    // ── load_skill_from_file ──────────────────────────────────────────────────

    #[test]
    fn load_with_frontmatter() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "review.md", "---\nname: code-review\ndescription: Reviews code\n---\nDo a thorough review.");
        let skill = load_skill_from_file(&tmp.path().join("review.md")).unwrap();
        assert_eq!(skill.name, "code-review");
        assert_eq!(skill.description, "Reviews code");
        assert!(skill.system_prompt.contains("thorough review"));
    }

    #[test]
    fn load_without_frontmatter_uses_filename() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "refactor.md", "Refactor the code.");
        let skill = load_skill_from_file(&tmp.path().join("refactor.md")).unwrap();
        assert_eq!(skill.name, "refactor");
        assert_eq!(skill.description, "");
        assert!(skill.system_prompt.contains("Refactor"));
    }

    #[test]
    fn load_missing_file_errors() {
        let result = load_skill_from_file(std::path::Path::new("/nonexistent/skill.md"));
        assert!(result.is_err());
    }

    // ── load_from_dir ─────────────────────────────────────────────────────────

    #[test]
    fn load_from_dir_empty_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let skills = load_from_dir(tmp.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn load_from_dir_nonexistent_returns_empty() {
        let skills = load_from_dir(std::path::Path::new("/nonexistent/skills")).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn load_from_dir_flat_md_files() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "a.md", "---\nname: alpha\n---\nAlpha skill");
        write_skill(tmp.path(), "b.md", "Beta skill");
        let skills = load_from_dir(tmp.path()).unwrap();
        assert_eq!(skills.len(), 2);
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"b"));
    }

    #[test]
    fn load_from_dir_bundled_directory_skill() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir(&skill_dir).unwrap();
        write_skill(&skill_dir, "skill.md", "---\nname: bundled\n---\nBundled skill body");
        let skills = load_from_dir(tmp.path()).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "bundled");
    }

    #[test]
    fn load_from_dir_ignores_non_md_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("README.txt"), "not a skill").unwrap();
        std::fs::write(tmp.path().join("config.yml"), "not a skill").unwrap();
        let skills = load_from_dir(tmp.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn load_from_dir_directory_without_skill_md_ignored() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("no-skill-md");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("other.md"), "not a skill").unwrap();
        let skills = load_from_dir(tmp.path()).unwrap();
        assert!(skills.is_empty());
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
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "foo.md", "---\nname: foo\n---\nFoo body");
        let skills = load_from_dir(tmp.path()).unwrap();
        let reg = SkillRegistry { skills };
        assert!(reg.find("foo").is_some());
        assert!(reg.find("bar").is_none());
    }

    #[test]
    fn registry_load_filtered_none_keeps_all() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "a.md", "A");
        write_skill(tmp.path(), "b.md", "B");
        let skills = load_from_dir(tmp.path()).unwrap();
        let mut reg = SkillRegistry { skills };
        // Simulate load_filtered(None) — no filtering
        // We test the retain logic directly
        let allowed: Option<&[String]> = None;
        if let Some(names) = allowed {
            reg.skills.retain(|s| names.iter().any(|n| n == &s.name));
        }
        assert_eq!(reg.all().len(), 2);
    }

    #[test]
    fn registry_load_filtered_empty_keeps_none() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "a.md", "A");
        let skills = load_from_dir(tmp.path()).unwrap();
        let mut reg = SkillRegistry { skills };
        let allowed: Vec<String> = vec![];
        reg.skills.retain(|s| allowed.iter().any(|n| n == &s.name));
        assert!(reg.all().is_empty());
    }

    #[test]
    fn registry_load_filtered_specific_names() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "a.md", "A");
        write_skill(tmp.path(), "b.md", "B");
        let skills = load_from_dir(tmp.path()).unwrap();
        let mut reg = SkillRegistry { skills };
        let allowed = vec!["a".to_string()];
        reg.skills.retain(|s| allowed.iter().any(|n| n == &s.name));
        assert_eq!(reg.all().len(), 1);
        assert_eq!(reg.all()[0].name, "a");
    }

    #[test]
    fn registry_project_overrides_global() {
        // Test the retain-then-push override logic from load()
        let skill_a = Skill {
            name: "coding".into(),
            description: "global version".into(),
            system_prompt: "global".into(),
            source: std::path::PathBuf::from("global/coding.md"),
        };
        let skill_b = Skill {
            name: "coding".into(),
            description: "project version".into(),
            system_prompt: "project".into(),
            source: std::path::PathBuf::from("project/coding.md"),
        };
        let mut skills = vec![skill_a];
        // Simulate project override
        skills.retain(|s| s.name != skill_b.name);
        skills.push(skill_b);
        let reg = SkillRegistry { skills };
        assert_eq!(reg.find("coding").unwrap().description, "project version");
        assert_eq!(reg.all().len(), 1);
    }

    // ── SkillRegistry::load() ─────────────────────────────────────────────────

    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TempHome {
        _dir: TempDir,
        orig: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let orig = std::env::var("HOME").ok();
            unsafe { std::env::set_var("HOME", dir.path()) };
            Self { _dir: dir, orig }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            unsafe {
                match &self.orig {
                    Some(h) => std::env::set_var("HOME", h),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    #[test]
    fn skill_registry_load_no_dirs_returns_empty() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();

        let reg = SkillRegistry::load().unwrap();
        assert!(reg.all().is_empty());

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn skill_registry_load_reads_global_skills() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();

        let global_dir = _home._dir.path().join(".cooper").join("skills");
        std::fs::create_dir_all(&global_dir).unwrap();
        write_skill(&global_dir, "global.md", "---\nname: global\n---\nGlobal skill");

        let reg = SkillRegistry::load().unwrap();
        assert!(reg.find("global").is_some());

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn skill_registry_load_project_overrides_global_skill() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();

        let global_dir = _home._dir.path().join(".cooper").join("skills");
        std::fs::create_dir_all(&global_dir).unwrap();
        write_skill(&global_dir, "tool.md", "---\nname: tool\n---\nGlobal tool skill");

        let project_dir = tmp_cwd.path().join(".agents").join("skills");
        std::fs::create_dir_all(&project_dir).unwrap();
        write_skill(&project_dir, "tool.md", "---\nname: tool\n---\nProject tool skill");

        let reg = SkillRegistry::load().unwrap();
        assert_eq!(reg.all().len(), 1);
        assert!(reg.find("tool").unwrap().system_prompt.contains("Project tool skill"));

        std::env::set_current_dir(prev).unwrap();
    }

    // ── SkillRegistry::load_filtered() ───────────────────────────────────────

    #[test]
    fn skill_registry_load_filtered_by_name() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();

        let global_dir = _home._dir.path().join(".cooper").join("skills");
        std::fs::create_dir_all(&global_dir).unwrap();
        write_skill(&global_dir, "a.md", "A skill");
        write_skill(&global_dir, "b.md", "B skill");

        let allowed = vec!["a".to_string()];
        let reg = SkillRegistry::load_filtered(Some(&allowed)).unwrap();
        assert_eq!(reg.all().len(), 1);
        assert_eq!(reg.all()[0].name, "a");

        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn skill_registry_load_filtered_none_keeps_all() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();

        let global_dir = _home._dir.path().join(".cooper").join("skills");
        std::fs::create_dir_all(&global_dir).unwrap();
        write_skill(&global_dir, "a.md", "A skill");
        write_skill(&global_dir, "b.md", "B skill");

        let reg = SkillRegistry::load_filtered(None).unwrap();
        assert_eq!(reg.all().len(), 2);

        std::env::set_current_dir(prev).unwrap();
    }

    // ── load_from_dir skips invalid frontmatter ───────────────────────────────

    #[test]
    fn load_from_dir_skips_invalid_frontmatter_with_warning() {
        let tmp = TempDir::new().unwrap();
        // YAML sequence as frontmatter — cannot be deserialized into SkillFrontmatter struct
        write_skill(tmp.path(), "bad.md", "---\n- list item\n---\nbody");
        let skills = load_from_dir(tmp.path()).unwrap();
        assert!(skills.is_empty());
    }
}
