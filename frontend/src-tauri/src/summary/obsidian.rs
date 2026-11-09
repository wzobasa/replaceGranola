//! Auto-export of generated summaries to an Obsidian vault (summary only, no transcript).

use crate::summary::llm_client::{generate_summary, LLMProvider};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tracing::{info, warn};

pub const DEFAULT_VAULT: &str = "/Users/wonuola/Documents/Claude playground/Obsidian Vault";
pub const DEFAULT_FOLDER: &str = "Meetings";
pub const FOLDERS: [&str; 4] = ["Meetings", "Customer interviews", "Wonu-Yareh meetings", "Vendors"];
const ROUTER_MODEL: &str = "claude-haiku-5-5";
const MAX_NAME_LEN: usize = 100;
const CONFIG_FILE: &str = "obsidian_export.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObsidianConfig {
    pub enabled: bool,
    pub vault_path: String,
}

impl Default for ObsidianConfig {
    fn default() -> Self {
        Self { enabled: true, vault_path: DEFAULT_VAULT.to_string() }
    }
}

fn config_path<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join(CONFIG_FILE))
}

pub fn load_config<R: Runtime>(app: &AppHandle<R>) -> ObsidianConfig {
    config_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[tauri::command]
pub async fn get_obsidian_export_config<R: Runtime>(app: AppHandle<R>) -> Result<ObsidianConfig, String> {
    Ok(load_config(&app))
}

#[tauri::command]
pub async fn set_obsidian_export_config<R: Runtime>(
    app: AppHandle<R>,
    config: ObsidianConfig,
) -> Result<(), String> {
    let path = config_path(&app).ok_or("no app data dir")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

// ---------- pure helpers ----------

/// Make a string safe as a macOS filename component.
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']') { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = collapsed.chars().take(MAX_NAME_LEN).collect();
    out = out.trim().trim_matches('.').trim().to_string();
    if out.is_empty() { "Untitled".to_string() } else { out }
}

/// True for auto-generated titles like "Meeting 09_11_26_07_40_45".
pub fn is_default_title(title: &str) -> bool {
    let t = title.trim();
    match t.strip_prefix("Meeting ") {
        Some(rest) => {
            let parts: Vec<&str> = rest.split('_').collect();
            parts.len() >= 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        }
        None => false,
    }
}

/// Extract the first balanced `{...}` object from text (ignores braces inside strings).
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    for (i, c) in text[start..].char_indices() {
        if in_str {
            if esc { esc = false } else if c == '\\' { esc = true } else if c == '"' { in_str = false }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse the router reply defensively. Unknown folder -> Meetings. Returns None if unparseable.
pub fn parse_routing(reply: &str) -> Option<(String, Option<String>)> {
    let obj = extract_json_object(reply)?;
    let v: serde_json::Value = serde_json::from_str(obj).ok()?;
    let folder_raw = v.get("folder").and_then(|f| f.as_str()).unwrap_or("").trim();
    let folder = FOLDERS
        .iter()
        .find(|f| f.eq_ignore_ascii_case(folder_raw))
        .copied()
        .unwrap_or(DEFAULT_FOLDER)
        .to_string();
    let title = v
        .get("title")
        .and_then(|t| t.as_str())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    Some((folder, title))
}

/// Read `meeting_id:` from a note's YAML frontmatter.
pub fn frontmatter_meeting_id(content: &str) -> Option<String> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let line = line.trim();
        if line == "---" {
            return None;
        }
        if let Some(rest) = line.strip_prefix("meeting_id:") {
            return Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
        }
    }
    None
}

pub fn build_note(date: &str, meeting_id: &str, title: &str, summary_md: &str) -> String {
    format!(
        "---\ndate: {}\nsource: replaceGranola\nmeeting_id: {}\n---\n\n# {}\n\n{}\n",
        date,
        meeting_id,
        title,
        summary_md.trim()
    )
}

/// Find an existing note for this meeting among candidate folders.
fn find_existing(vault: &Path, meeting_id: &str) -> Option<PathBuf> {
    for folder in FOLDERS {
        let Ok(entries) = std::fs::read_dir(vault.join(folder)) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&p) {
                if frontmatter_meeting_id(&content).as_deref() == Some(meeting_id) {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Pick a path that does not hold a different meeting's note.
fn unique_path(dir: &Path, base: &str, meeting_id: &str) -> PathBuf {
    let mut n = 1;
    loop {
        let name = if n == 1 { format!("{base}.md") } else { format!("{base} ({n}).md") };
        let p = dir.join(name);
        match std::fs::read_to_string(&p) {
            Err(_) if !p.exists() => return p,
            Ok(c) if frontmatter_meeting_id(&c).as_deref() == Some(meeting_id) => return p,
            _ => n += 1,
        }
    }
}

/// Write (or overwrite in place) the note; returns the path written.
pub fn write_note(
    vault: &Path,
    folder: &str,
    date: &str,
    title: &str,
    meeting_id: &str,
    summary_md: &str,
) -> Result<PathBuf, String> {
    let content = build_note(date, meeting_id, title, summary_md);
    let path = match find_existing(vault, meeting_id) {
        Some(p) => p,
        None => {
            let dir = vault.join(folder);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            unique_path(&dir, &format!("{} {}", date, sanitize_filename(title)), meeting_id)
        }
    };
    std::fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(path)
}

// ---------- orchestration ----------

async fn route_with_claude(
    api_key: &str,
    title: &str,
    summary_md: &str,
) -> Result<(String, Option<String>), String> {
    let system = "You file meeting notes. Reply with ONLY strict JSON: {\"folder\": <one of the folders>, \"title\": <short descriptive title>}. \
Folders: \"Meetings\" (default/general), \"Customer interviews\" (interviews or discovery calls with customers/users/prospects), \
\"Wonu-Yareh meetings\" (meetings between Wonu and her co-founder/CTO Yareh), \"Vendors\" (calls with vendors, suppliers or service providers).";
    let excerpt: String = summary_md.chars().take(6000).collect();
    let user = format!("Meeting title: {title}\n\nSummary:\n{excerpt}");
    let client = reqwest::Client::new();
    let completion = generate_summary(
        &client, &LLMProvider::Claude, ROUTER_MODEL, api_key, system, &user,
        None, None, Some(300), None, None, None, None,
    )
    .await?;
    parse_routing(&completion.content).ok_or_else(|| "unparseable routing reply".to_string())
}

/// Export a freshly generated summary. Never panics or errors out to the caller; emits
/// `obsidian-export` ({ok, message}) for the frontend toast.
pub async fn export_summary<R: Runtime>(
    app: &AppHandle<R>,
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    summary_md: &str,
) {
    let cfg = load_config(app);
    if !cfg.enabled {
        return;
    }
    let result: Result<String, String> = async {
        let meeting = crate::database::repositories::meeting::MeetingsRepository::get_meeting_metadata(pool, meeting_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("meeting not found")?;
        let api_key = crate::database::repositories::setting::SettingsRepository::get_api_key(pool, "claude")
            .await
            .ok()
            .flatten()
            .filter(|k| !k.trim().is_empty());

        let mut folder = DEFAULT_FOLDER.to_string();
        let mut title = meeting.title.clone();
        if let Some(key) = api_key {
            match route_with_claude(key.trim(), &meeting.title, summary_md).await {
                Ok((f, t)) => {
                    folder = f;
                    if is_default_title(&meeting.title) {
                        if let Some(t) = t {
                            title = t;
                        }
                    }
                }
                Err(e) => warn!("Obsidian routing failed, using defaults: {e}"),
            }
        }
        let date = meeting.created_at.0.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string();
        let path = write_note(Path::new(&cfg.vault_path), &folder, &date, &title, meeting_id, summary_md)?;
        info!("Obsidian note written: {}", path.display());
        let shown = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or(&folder)
            .to_string();
        Ok(shown)
    }
    .await;

    let payload = match result {
        Ok(folder) => serde_json::json!({"ok": true, "message": format!("Saved to Obsidian › {folder}")}),
        Err(e) => {
            warn!("Obsidian export failed: {e}");
            serde_json::json!({"ok": false, "message": format!("Obsidian export failed: {e}")})
        }
    };
    let _ = app.emit("obsidian-export", payload);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_unsafe_and_caps() {
        assert_eq!(sanitize_filename("a/b:c*d?"), "a b c d");
        assert_eq!(sanitize_filename("  ..  "), "Untitled");
        assert_eq!(sanitize_filename(&"x".repeat(300)).chars().count(), 100);
    }

    #[test]
    fn default_title_detection() {
        assert!(is_default_title("Meeting 09_11_26_07_40_45"));
        assert!(!is_default_title("Meeting with Yareh"));
        assert!(!is_default_title("Q3 planning"));
    }

    #[test]
    fn parse_fenced_and_noisy_json() {
        let r = "```json\n{\"folder\": \"Vendors\", \"title\": \"Acme call\"}\n```";
        assert_eq!(parse_routing(r), Some(("Vendors".into(), Some("Acme call".into()))));
        let r = "Sure! {\"folder\":\"customer interviews\",\"title\":\"A {b}\"} done";
        assert_eq!(parse_routing(r), Some(("Customer interviews".into(), Some("A {b}".into()))));
    }

    #[test]
    fn parse_unknown_folder_and_garbage() {
        assert_eq!(parse_routing("{\"folder\":\"Nope\"}"), Some(("Meetings".into(), None)));
        assert_eq!(parse_routing("no json here"), None);
    }

    #[test]
    fn frontmatter_id_match() {
        let n = build_note("2026-11-09", "abc-123", "T", "body");
        assert_eq!(frontmatter_meeting_id(&n).as_deref(), Some("abc-123"));
        assert_eq!(frontmatter_meeting_id("# no frontmatter\nmeeting_id: x"), None);
    }

    #[test]
    fn write_is_idempotent_and_collision_safe() {
        let dir = tempfile::tempdir().unwrap();
        let v = dir.path();
        let p1 = write_note(v, "Meetings", "2026-11-09", "Sync", "id1", "one").unwrap();
        let p1b = write_note(v, "Vendors", "2026-11-09", "Sync renamed", "id1", "two").unwrap();
        assert_eq!(p1, p1b);
        assert!(std::fs::read_to_string(&p1).unwrap().contains("two"));
        let p2 = write_note(v, "Meetings", "2026-11-09", "Sync", "id2", "other").unwrap();
        assert_ne!(p1, p2);
        assert!(p2.to_string_lossy().ends_with("Sync (2).md"));
    }
}
