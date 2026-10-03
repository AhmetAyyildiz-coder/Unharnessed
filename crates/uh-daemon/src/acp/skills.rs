//! The skills on the settings screen: the `_unharnessed/skills/*`
//! extension methods. They read the skill folders and the settings afresh
//! on each call, and write the global settings file only.
//!
//! - `_unharnessed/skills/list` answers
//!   `{ skills, folders, problems, restart }`. Each skill is
//!   `{ name, description, path, location, source, enabled, active }`:
//!   `path` is its file, `location` its folder (`~` for the home folder),
//!   `source` is `builtin`, `user` (the agent dir's and `~/.agents` skills
//!   folders), `folder` (a folder the `skills` setting adds) or `project`;
//!   `enabled`: the agent loads it when it starts; `active`: the running
//!   agent has it. `folders` are the folders the global `skills` setting
//!   adds, `problems` (`{ message, path? }`) what keeps a skill from
//!   loading, and `restart` whether the next start loads other skills than
//!   the running agent has.
//! - `_unharnessed/skills/set` `{ path, enabled }` turns the skill whose
//!   file is `path` on or off: off writes `-<its folder>` in the global
//!   `skills` setting, on removes that entry (and writes `+<its folder>`
//!   when another entry still turns it off).
//! - `_unharnessed/skills/create` `{ name, description }` writes
//!   `<agent dir>/skills/<name>/SKILL.md`; the answer adds `created`, the
//!   file's path.
//! - `_unharnessed/skills/add_folder` and `_unharnessed/skills/remove_folder`
//!   `{ path }` add or remove a folder of the global `skills` setting.
//!
//! Every method but `list` answers like `list`, or is refused with `-32602`
//! and `data.details`. Skills load when the agent starts: a change applies
//! at the next start.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use uh_core::resource_paths::is_pattern;
use uh_core::resources::{list_skills, ResourceLoaderOptions, SkillListing};
use uh_core::session_engine::engine::builtin_skill_overrides;
use uh_core::settings::SettingsManager;
use uh_core::skills::{ResourceDiagnostic, Skill};

use super::jsonrpc;
use super::producer::FrameSink;

pub(super) const LIST: &str = "_unharnessed/skills/list";
pub(super) const SET: &str = "_unharnessed/skills/set";
pub(super) const CREATE: &str = "_unharnessed/skills/create";
pub(super) const ADD_FOLDER: &str = "_unharnessed/skills/add_folder";
pub(super) const REMOVE_FOLDER: &str = "_unharnessed/skills/remove_folder";

enum Action {
    List,
    Set { path: PathBuf, enabled: bool },
    Create { name: String, description: String },
    AddFolder(String),
    RemoveFolder(String),
}

/// Serve a `_unharnessed/skills/*` request; `false` when `method` is not
/// one. `running` are the skills the running agent loaded.
pub(super) async fn handle_request(
    id: &Value,
    method: &str,
    params: &Value,
    cwd: &Arc<PathBuf>,
    agent_dir: &Arc<PathBuf>,
    running: &[Skill],
    tx: &FrameSink,
) -> bool {
    let action = match method {
        LIST => Ok(Action::List),
        SET => string_param(params, "path").and_then(|path| {
            let enabled = params
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| "enabled must be true or false".to_string())?;
            Ok(Action::Set {
                path: PathBuf::from(path),
                enabled,
            })
        }),
        CREATE => string_param(params, "name").and_then(|name| {
            Ok(Action::Create {
                name,
                description: string_param(params, "description")?,
            })
        }),
        ADD_FOLDER => string_param(params, "path").map(Action::AddFolder),
        REMOVE_FOLDER => string_param(params, "path").map(Action::RemoveFolder),
        _ => return false,
    };
    let action = match action {
        Ok(action) => action,
        Err(details) => {
            let _ = tx.send(invalid_params(id, &details));
            return true;
        }
    };
    let active: HashSet<PathBuf> = running
        .iter()
        .map(|skill| skill.file_path.clone())
        .collect();
    // Folder scans and file writes run off the async loop.
    let task = tokio::task::spawn_blocking({
        let (cwd, agent_dir) = (Arc::clone(cwd), Arc::clone(agent_dir));
        move || {
            let created = apply(&cwd, &agent_dir, action)?;
            let mut view = view(&cwd, &agent_dir, &active);
            if let Some(created) = created {
                view["created"] = json!(created.display().to_string());
            }
            Ok::<Value, String>(view)
        }
    });
    let answer = match task.await {
        Ok(Ok(view)) => jsonrpc::response(id, &view),
        Ok(Err(details)) => invalid_params(id, &details),
        Err(_) => jsonrpc::error_response(id, jsonrpc::INTERNAL_ERROR, "skills task failed", None),
    };
    let _ = tx.send(answer);
    true
}

/// Run the action; `create` gives the new file.
fn apply(cwd: &Path, agent_dir: &Path, action: Action) -> Result<Option<PathBuf>, String> {
    match action {
        Action::List => {}
        Action::Set { path, enabled } => set_enabled(cwd, agent_dir, &path, enabled)?,
        Action::Create { name, description } => {
            return create(cwd, agent_dir, &name, &description).map(Some);
        }
        Action::AddFolder(folder) => {
            let folder = folder.trim().to_string();
            if is_pattern(&folder) {
                return Err("a folder path cannot start with +, - or !, nor hold * or ?".into());
            }
            let resolved = expand_home(&folder)
                .filter(|path| path.is_absolute())
                .ok_or("give the folder's full path, or one starting with ~")?;
            if !resolved.is_dir() {
                return Err(format!("no folder at {folder}"));
            }
            let mut entries = skills_setting(cwd, agent_dir);
            if !entries.contains(&folder) {
                entries.push(folder);
                write_skills_setting(cwd, agent_dir, &entries)?;
            }
        }
        Action::RemoveFolder(folder) => {
            let mut entries = skills_setting(cwd, agent_dir);
            entries.retain(|entry| *entry != folder);
            write_skills_setting(cwd, agent_dir, &entries)?;
        }
    }
    Ok(None)
}

/// The skills as the agent resolves them when it starts.
fn listing(cwd: &Path, agent_dir: &Path) -> SkillListing {
    let settings = SettingsManager::create(cwd, agent_dir);
    let compaction = settings.settings().compaction.clone().unwrap_or_default();
    list_skills(ResourceLoaderOptions {
        settings: Some(settings),
        extra_builtin_skill_overrides: builtin_skill_overrides(&compaction),
        ..ResourceLoaderOptions::new(cwd, agent_dir)
    })
}

/// The answer to every method.
fn view(cwd: &Path, agent_dir: &Path, active: &HashSet<PathBuf>) -> Value {
    let listing = listing(cwd, agent_dir);
    let skills: Vec<Value> = listing
        .skills
        .iter()
        .map(|listed| {
            let skill = &listed.skill;
            json!({
                "name": skill.name,
                "description": skill.description,
                "path": skill.file_path.display().to_string(),
                "location": shown(&target(&skill.file_path)),
                "source": source(skill),
                "enabled": listed.enabled,
                "active": active.contains(&skill.file_path),
            })
        })
        .collect();
    let enabled: HashSet<&PathBuf> = listing
        .skills
        .iter()
        .filter(|listed| listed.enabled)
        .map(|listed| &listed.skill.file_path)
        .collect();
    let restart = enabled != active.iter().collect();
    let folders: Vec<String> = skills_setting(cwd, agent_dir)
        .into_iter()
        .filter(|entry| !is_pattern(entry))
        .collect();
    let problems: Vec<Value> = listing.diagnostics.iter().map(problem).collect();
    json!({ "skills": skills, "folders": folders, "problems": problems, "restart": restart })
}

/// Where the skill comes from, as the screen groups them.
fn source(skill: &Skill) -> &'static str {
    let info = &skill.source_info;
    match (info.source.as_str(), info.scope) {
        ("builtin", _) => "builtin",
        (_, uh_core::skills::SourceScope::Project) => "project",
        ("local", _) => "folder",
        _ => "user",
    }
}

fn problem(diagnostic: &ResourceDiagnostic) -> Value {
    let (message, path) = match diagnostic {
        ResourceDiagnostic::Warning { message, path }
        | ResourceDiagnostic::Error { message, path } => (message.clone(), path.clone()),
        ResourceDiagnostic::Collision { message, path, .. } => {
            (message.clone(), Some(path.clone()))
        }
    };
    let mut problem = json!({ "message": message });
    if let Some(path) = path {
        problem["path"] = json!(shown(Path::new(&path)));
    }
    problem
}

/// What a settings entry names for this skill: its folder, or the file
/// itself when it is a loose `.md` skill.
fn target(file: &Path) -> PathBuf {
    match (file.file_name(), file.parent()) {
        (Some(name), Some(folder)) if name == "SKILL.md" => folder.to_path_buf(),
        _ => file.to_path_buf(),
    }
}

fn set_enabled(cwd: &Path, agent_dir: &Path, file: &Path, enabled: bool) -> Result<(), String> {
    let is_on = || {
        listing(cwd, agent_dir)
            .skills
            .iter()
            .find(|listed| listed.skill.file_path == file)
            .map(|listed| listed.enabled)
    };
    if is_on().is_none() {
        return Err(format!("no skill at {}", file.display()));
    }
    let target = target(file).display().to_string();
    let (off, on) = (format!("-{target}"), format!("+{target}"));
    let mut entries = skills_setting(cwd, agent_dir);
    entries.retain(|entry| *entry != off && *entry != on);
    if !enabled {
        entries.push(off);
    }
    write_skills_setting(cwd, agent_dir, &entries)?;
    if enabled && is_on() == Some(false) {
        // Another entry turns it off (a `!` pattern): a `+` brings it back,
        // unless that entry is a `-` one, which wins.
        entries.push(on);
        write_skills_setting(cwd, agent_dir, &entries)?;
        if is_on() == Some(false) {
            entries.pop();
            write_skills_setting(cwd, agent_dir, &entries)?;
            return Err("another entry of the skills setting turns it off".into());
        }
    }
    Ok(())
}

fn create(cwd: &Path, agent_dir: &Path, name: &str, description: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    let description = description.trim();
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if !valid {
        return Err("a skill name is 1 to 64 lowercase letters, digits and single hyphens, with no hyphen at either end".into());
    }
    if description.is_empty() {
        return Err("a skill needs a description: the agent picks skills by it".into());
    }
    if description.chars().count() > 1024 {
        return Err("a skill description is at most 1024 characters".into());
    }
    if let Some(same) = listing(cwd, agent_dir)
        .skills
        .iter()
        .find(|listed| listed.skill.name == name)
    {
        return Err(format!(
            "a skill named {name} already exists: {}",
            shown(&same.skill.file_path)
        ));
    }
    let folder = agent_dir.join("skills").join(name);
    if folder.exists() {
        return Err(format!("{} already exists", shown(&folder)));
    }
    let file = folder.join("SKILL.md");
    // A JSON string is a YAML double-quoted scalar: any description fits.
    let content = format!(
        "---\nname: {name}\ndescription: {}\n---\n\n# {name}\n\nWhat the agent should know and do when this skill applies.\n",
        serde_json::to_string(description).unwrap_or_default()
    );
    std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(&file, content))
        .map_err(|error| format!("could not write {}: {error}", shown(&file)))?;
    Ok(file)
}

/// The global `skills` setting's entries.
fn skills_setting(cwd: &Path, agent_dir: &Path) -> Vec<String> {
    SettingsManager::create(cwd, agent_dir)
        .global_settings()
        .skills
        .clone()
        .unwrap_or_default()
}

fn write_skills_setting(cwd: &Path, agent_dir: &Path, entries: &[String]) -> Result<(), String> {
    let value = (!entries.is_empty()).then(|| json!(entries));
    SettingsManager::create(cwd, agent_dir)
        .set_global_value(&["skills"], value.as_ref())
        .map_err(|error| error.to_string())
}

fn home() -> Option<PathBuf> {
    uh_types::platform::home_dir()
}

/// `~` and `~/…` against the home folder; other paths as given.
fn expand_home(path: &str) -> Option<PathBuf> {
    match path.strip_prefix('~') {
        Some("") => home(),
        Some(rest) => rest
            .strip_prefix('/')
            .and_then(|rest| Some(home()?.join(rest))),
        None => Some(PathBuf::from(path)),
    }
}

/// The path with `~` for the home folder.
fn shown(path: &Path) -> String {
    match home().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn string_param(params: &Value, name: &str) -> Result<String, String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{name} must be a non-empty string"))
}

fn invalid_params(id: &Value, details: &str) -> Value {
    jsonrpc::error_response(
        id,
        jsonrpc::INVALID_PARAMS,
        "Invalid params",
        Some(&json!({ "details": details })),
    )
}
