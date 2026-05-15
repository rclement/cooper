use anyhow::{Context, Result};
use cooper_core::{Skill, SkillRegistry, parse_skill};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

// ── LoadedSkills ──────────────────────────────────────────────────────────────

/// CLI-side registry: wraps the core `SkillRegistry` and tracks the bundle
/// directory for skills loaded from a directory (the `skills/name/skill.md`
/// convention). Flat-file skills have no bundle directory entry.
pub struct LoadedSkills {
    pub registry: SkillRegistry,
    bundle_dirs: HashMap<String, PathBuf>,
}

impl LoadedSkills {
    pub(crate) fn from_pairs(pairs: Vec<(Skill, Option<PathBuf>)>) -> Self {
        let mut bundle_dirs = HashMap::new();
        let skills = pairs
            .into_iter()
            .map(|(skill, dir)| {
                if let Some(d) = dir {
                    bundle_dirs.insert(skill.name.clone(), d);
                }
                skill
            })
            .collect();
        Self { registry: SkillRegistry::new(skills), bundle_dirs }
    }

    pub fn empty() -> Self {
        Self { registry: SkillRegistry::empty(), bundle_dirs: HashMap::new() }
    }

    /// Returns the bundle directory for a bundled skill, or `None` for flat-file skills.
    pub fn bundle_dir(&self, name: &str) -> Option<&Path> {
        self.bundle_dirs.get(name).map(PathBuf::as_path)
    }
}

impl std::ops::Deref for LoadedSkills {
    type Target = SkillRegistry;
    fn deref(&self) -> &SkillRegistry {
        &self.registry
    }
}

// ── File I/O ──────────────────────────────────────────────────────────────────

fn load_skill_from_file(path: &Path) -> Result<Skill> {
    let content =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let name_hint = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    parse_skill(&content, &name_hint)
        .with_context(|| format!("parsing skill at {}", path.display()))
}

/// Returns `(skill, bundle_dir)` pairs. `bundle_dir` is `Some` only for skills
/// loaded from a `name/skill.md` directory structure.
fn load_from_dir(dir: &Path) -> Result<Vec<(Skill, Option<PathBuf>)>> {
    let mut pairs = Vec::new();

    let mut entries: Vec<_> = match fs::read_dir(dir) {
        Ok(iter) => iter.filter_map(|e| e.ok()).collect(),
        Err(_) => return Ok(pairs),
    };
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let (skill_path, bundle_dir) = if path.is_file()
            && path.extension().map_or(false, |e| e == "md")
        {
            (path.clone(), None)
        } else if path.is_dir() {
            let candidate = path.join("skill.md");
            if candidate.exists() {
                (candidate, Some(path.clone()))
            } else {
                continue;
            }
        } else {
            continue;
        };

        match load_skill_from_file(&skill_path) {
            Ok(skill) => pairs.push((skill, bundle_dir)),
            Err(e) => eprintln!("warning: skipping invalid skill definition: {}", e),
        }
    }

    Ok(pairs)
}

// ── Public loaders ────────────────────────────────────────────────────────────

impl LoadedSkills {
    /// Loads skills from global (`~/.cooper/skills`) and project (`.agents/skills`) directories.
    /// Project skills override globals of the same name.
    pub fn load() -> Result<Self> {
        let mut pairs: Vec<(Skill, Option<PathBuf>)> = Vec::new();

        if let Some(home) = dirs::home_dir() {
            pairs.extend(load_from_dir(&home.join(".cooper").join("skills"))?);
        }

        for (skill, dir) in load_from_dir(&PathBuf::from(".agents/skills"))? {
            pairs.retain(|(s, _)| s.name != skill.name);
            pairs.push((skill, dir));
        }

        Ok(Self::from_pairs(pairs))
    }

    /// Like `load()` but retains only the skills named in `allowed`.
    /// `None` means all skills are kept; `Some(&[])` means none.
    pub fn load_filtered(allowed: Option<&[String]>) -> Result<Self> {
        let mut loaded = Self::load()?;
        if let Some(names) = allowed {
            loaded.registry = SkillRegistry::new(
                loaded.registry.all().iter()
                    .filter(|s| names.iter().any(|n| n == &s.name))
                    .cloned()
                    .collect(),
            );
            loaded.bundle_dirs.retain(|k, _| names.iter().any(|n| n == k));
        }
        Ok(loaded)
    }
}

// ── Test helper ───────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) fn load_from_dir_pub(dir: &Path) -> Result<LoadedSkills> {
    Ok(LoadedSkills::from_pairs(load_from_dir(dir)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_skill(dir: &Path, filename: &str, content: &str) {
        std::fs::write(dir.join(filename), content).unwrap();
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
        assert!(load_skill_from_file(Path::new("/nonexistent/skill.md")).is_err());
    }

    // ── load_from_dir ─────────────────────────────────────────────────────────

    #[test]
    fn load_from_dir_empty_returns_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(load_from_dir(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn load_from_dir_nonexistent_returns_empty() {
        assert!(load_from_dir(Path::new("/nonexistent/skills")).unwrap().is_empty());
    }

    #[test]
    fn load_from_dir_flat_md_files() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "a.md", "---\nname: alpha\n---\nAlpha skill");
        write_skill(tmp.path(), "b.md", "Beta skill");
        let pairs = load_from_dir(tmp.path()).unwrap();
        assert_eq!(pairs.len(), 2);
        let names: Vec<_> = pairs.iter().map(|(s, _)| s.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"b"));
        // flat files have no bundle dir
        assert!(pairs.iter().all(|(_, d)| d.is_none()));
    }

    #[test]
    fn load_from_dir_bundled_skill_has_dir() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir(&skill_dir).unwrap();
        write_skill(&skill_dir, "skill.md", "---\nname: bundled\n---\nBundled skill body");
        let pairs = load_from_dir(tmp.path()).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0.name, "bundled");
        assert_eq!(pairs[0].1.as_deref(), Some(skill_dir.as_path()));
    }

    #[test]
    fn load_from_dir_ignores_non_md_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("README.txt"), "not a skill").unwrap();
        assert!(load_from_dir(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn load_from_dir_directory_without_skill_md_ignored() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("no-skill-md");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("other.md"), "not a skill").unwrap();
        assert!(load_from_dir(tmp.path()).unwrap().is_empty());
    }

    // ── LoadedSkills ──────────────────────────────────────────────────────────

    #[test]
    fn loaded_skills_empty() {
        let ls = LoadedSkills::empty();
        assert!(ls.all().is_empty());
        assert!(ls.find("anything").is_none());
        assert!(ls.bundle_dir("anything").is_none());
    }

    #[test]
    fn loaded_skills_bundle_dir_tracked() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir(&skill_dir).unwrap();
        write_skill(&skill_dir, "skill.md", "---\nname: my-skill\n---\nContent");
        write_skill(tmp.path(), "flat.md", "Flat skill");
        let pairs = load_from_dir(tmp.path()).unwrap();
        let ls = LoadedSkills::from_pairs(pairs);
        assert!(ls.bundle_dir("my-skill").is_some());
        assert!(ls.bundle_dir("flat").is_none());
    }

    #[test]
    fn load_from_dir_skips_invalid_frontmatter_with_warning() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "bad.md", "---\n- list item\n---\nbody");
        assert!(load_from_dir(tmp.path()).unwrap().is_empty());
    }

    // ── LoadedSkills::load / load_filtered ────────────────────────────────────

    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TempHome { _dir: TempDir, orig: Option<String> }
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
    fn load_no_dirs_returns_empty() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();
        assert!(LoadedSkills::load().unwrap().all().is_empty());
        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn load_reads_global_skills() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();
        let global_dir = _home._dir.path().join(".cooper").join("skills");
        std::fs::create_dir_all(&global_dir).unwrap();
        write_skill(&global_dir, "global.md", "---\nname: global\n---\nGlobal skill");
        assert!(LoadedSkills::load().unwrap().find("global").is_some());
        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn load_project_overrides_global_skill() {
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
        let ls = LoadedSkills::load().unwrap();
        assert_eq!(ls.all().len(), 1);
        assert!(ls.find("tool").unwrap().system_prompt.contains("Project tool skill"));
        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn load_filtered_by_name() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();
        let global_dir = _home._dir.path().join(".cooper").join("skills");
        std::fs::create_dir_all(&global_dir).unwrap();
        write_skill(&global_dir, "a.md", "A skill");
        write_skill(&global_dir, "b.md", "B skill");
        let ls = LoadedSkills::load_filtered(Some(&["a".to_string()])).unwrap();
        assert_eq!(ls.all().len(), 1);
        assert_eq!(ls.all()[0].name, "a");
        std::env::set_current_dir(prev).unwrap();
    }

    #[test]
    fn load_filtered_none_keeps_all() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _home = TempHome::new();
        let prev = std::env::current_dir().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        std::env::set_current_dir(tmp_cwd.path()).unwrap();
        let global_dir = _home._dir.path().join(".cooper").join("skills");
        std::fs::create_dir_all(&global_dir).unwrap();
        write_skill(&global_dir, "a.md", "A skill");
        write_skill(&global_dir, "b.md", "B skill");
        assert_eq!(LoadedSkills::load_filtered(None).unwrap().all().len(), 2);
        std::env::set_current_dir(prev).unwrap();
    }
}
