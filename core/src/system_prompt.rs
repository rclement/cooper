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

    match (opts.date.as_deref(), opts.cwd.as_deref()) {
        (Some(d), Some(c)) => system.push_str(&format!("\n\nCurrent date: {d}\nCurrent working directory: {c}")),
        (Some(d), None)    => system.push_str(&format!("\n\nCurrent date: {d}")),
        (None,    Some(c)) => system.push_str(&format!("\n\nCurrent working directory: {c}")),
        (None,    None)    => {}
    }

    system
}
