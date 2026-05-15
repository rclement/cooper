pub const DEFAULT: &str =
    "You are a helpful AI assistant operating inside cooper, a coding agent harness.";

pub struct ContextFile {
    pub path: String,
    pub content: String,
}

pub struct SkillInfo {
    pub name: String,
    pub description: String,
}

pub struct Options {
    pub base: String,
    pub date: Option<String>,
    pub cwd: Option<String>,
    pub skills: Vec<SkillInfo>,
    pub agent_instructions: Option<String>,
    pub context_files: Vec<ContextFile>,
}

pub fn build(opts: Options) -> String {
    let mut system = opts.base;

    if !opts.skills.is_empty() {
        let names: Vec<&str> = opts.skills.iter().map(|s| s.name.as_str()).collect();
        system.push_str(&format!(
            "\n\nYou have access to skill modules ({}) via the `activate_skill` tool. \
            Activate the most relevant skill at the start of any task that matches its domain.",
            names.join(", ")
        ));
    }

    if let Some(instructions) = opts.agent_instructions {
        system.push_str(&format!(
            "\n\n<agent-instructions>\n{}\n</agent-instructions>",
            instructions.trim_end()
        ));
    }

    if !opts.context_files.is_empty() {
        let mut ctx = String::new();
        for f in opts.context_files {
            ctx.push_str(&format!("<file path=\"{}\">\n{}\n</file>\n", f.path, f.content));
        }
        system.push_str(&format!("\n\n<context>\n{}</context>", ctx));
    }

    match (opts.date.as_deref(), opts.cwd.as_deref()) { // all four combinations for coverage
        (Some(d), Some(c)) => system.push_str(&format!("\n\nCurrent date: {d}\nCurrent working directory: {c}")),
        (Some(d), None)    => system.push_str(&format!("\n\nCurrent date: {d}")),
        (None,    Some(c)) => system.push_str(&format!("\n\nCurrent working directory: {c}")),
        (None,    None)    => {}
    }

    system
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_opts() -> Options {
        Options {
            base: "base".to_string(),
            date: None,
            cwd: None,
            skills: vec![],
            agent_instructions: None,
            context_files: vec![],
        }
    }

    #[test]
    fn default_constant_nonempty() {
        assert!(!DEFAULT.is_empty());
    }

    #[test]
    fn build_base_only() {
        assert_eq!(build(base_opts()), "base");
    }

    #[test]
    fn build_with_skills() {
        let mut opts = base_opts();
        opts.skills = vec![
            SkillInfo { name: "coding".into(), description: "writes code".into() },
        ];
        let result = build(opts);
        assert!(result.contains("activate_skill"));
        assert!(result.contains("coding"));
    }

    #[test]
    fn build_with_agent_instructions() {
        let mut opts = base_opts();
        opts.agent_instructions = Some("  do things  \n".into());
        let result = build(opts);
        assert!(result.contains("<agent-instructions>"));
        assert!(result.contains("do things"));
        assert!(result.contains("</agent-instructions>"));
        // trim_end is applied
        assert!(!result.contains("do things  \n</agent-instructions>"));
    }

    #[test]
    fn build_with_context_files() {
        let mut opts = base_opts();
        opts.context_files = vec![ContextFile { path: "notes.md".into(), content: "important".into() }];
        let result = build(opts);
        assert!(result.contains("<context>"));
        assert!(result.contains("<file path=\"notes.md\">"));
        assert!(result.contains("important"));
        assert!(result.contains("</context>"));
    }

    #[test]
    fn build_date_and_cwd() {
        let mut opts = base_opts();
        opts.date = Some("2024-01-15".into());
        opts.cwd = Some("/home/user/project".into());
        let result = build(opts);
        assert!(result.contains("Current date: 2024-01-15"));
        assert!(result.contains("Current working directory: /home/user/project"));
    }

    #[test]
    fn build_date_only() {
        let mut opts = base_opts();
        opts.date = Some("2024-06-01".into());
        let result = build(opts);
        assert!(result.contains("Current date: 2024-06-01"));
        assert!(!result.contains("working directory"));
    }

    #[test]
    fn build_cwd_only() {
        let mut opts = base_opts();
        opts.cwd = Some("/tmp/proj".into());
        let result = build(opts);
        assert!(!result.contains("Current date"));
        assert!(result.contains("Current working directory: /tmp/proj"));
    }

    #[test]
    fn build_no_date_no_cwd() {
        let result = build(base_opts());
        assert!(!result.contains("Current date"));
        assert!(!result.contains("working directory"));
    }

    #[test]
    fn build_skill_with_empty_description() {
        let mut opts = base_opts();
        opts.skills = vec![SkillInfo { name: "bare".into(), description: "".into() }];
        let result = build(opts);
        // skill name still appears in the list
        assert!(result.contains("bare"));
    }

    #[test]
    fn build_multiple_context_files() {
        let mut opts = base_opts();
        opts.context_files = vec![
            ContextFile { path: "a.md".into(), content: "aaa".into() },
            ContextFile { path: "b.md".into(), content: "bbb".into() },
        ];
        let result = build(opts);
        assert!(result.contains("a.md"));
        assert!(result.contains("b.md"));
    }
}
