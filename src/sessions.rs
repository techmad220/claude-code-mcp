//! Claude Code session history parser
//!
//! Claude Code stores sessions in ~/.claude/projects/<project-hash>/<session-id>.jsonl
//! Each line is a JSON object with type, message, timestamp, sessionId fields.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// A Claude Code session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project_path: Option<String>,
    pub cwd: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub messages: Vec<Message>,
    pub file_path: PathBuf,
}

/// A message in a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

/// Summary of a session for listing
#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub project_path: Option<String>,
    pub cwd: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub message_count: usize,
    pub preview: String,
}

/// Context summary of a session
#[derive(Debug, Serialize)]
pub struct SessionContext {
    pub id: String,
    pub cwd: Option<String>,
    pub initial_request: Option<String>,
    pub message_count: usize,
    pub files_mentioned: Vec<String>,
    pub key_terms: Vec<String>,
}

/// Claude Code session storage handler
pub struct SessionStore {
    base_path: PathBuf,
}

impl SessionStore {
    /// Create a new session store, finding the Claude Code directory
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().context("Could not find home directory")?;
        let claude_dir = home.join(".claude");

        if !claude_dir.exists() {
            anyhow::bail!(
                "Claude Code directory not found at ~/.claude. \
                 Make sure Claude Code CLI is installed and has been used at least once."
            );
        }

        Ok(Self {
            base_path: claude_dir,
        })
    }

    /// List all sessions, sorted by recency
    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let mut sessions = Vec::new();
        let projects_dir = self.base_path.join("projects");

        if !projects_dir.exists() {
            return Ok(sessions);
        }

        // Walk through the projects directory looking for .jsonl session files
        for entry in WalkDir::new(&projects_dir)
            .max_depth(3)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
                // Skip agent files (subagent sessions)
                if path.file_name().map_or(false, |n| n.to_string_lossy().starts_with("agent-")) {
                    continue;
                }
                if let Ok(Some(session)) = self.try_parse_jsonl_session(path) {
                    sessions.push(session_to_summary(&session));
                }
            }
        }

        // Sort by updated_at descending (most recent first)
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        // Apply limit
        sessions.truncate(limit.min(100));

        Ok(sessions)
    }

    /// Search sessions by keyword
    pub fn search_sessions(&self, query: &str, limit: usize) -> Result<Vec<SessionSummary>> {
        let matcher = SkimMatcherV2::default();
        let mut results: Vec<(i64, SessionSummary)> = Vec::new();
        let projects_dir = self.base_path.join("projects");

        if !projects_dir.exists() {
            return Ok(vec![]);
        }

        for entry in WalkDir::new(&projects_dir)
            .max_depth(3)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
                if path.file_name().map_or(false, |n| n.to_string_lossy().starts_with("agent-")) {
                    continue;
                }
                if let Ok(Some(session)) = self.try_parse_jsonl_session(path) {
                    // Search through all message content
                    let full_text: String = session
                        .messages
                        .iter()
                        .map(|m| m.content.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");

                    if let Some(score) = matcher.fuzzy_match(&full_text, query) {
                        results.push((score, session_to_summary(&session)));
                    }
                }
            }
        }

        // Sort by match score descending
        results.sort_by(|a, b| b.0.cmp(&a.0));

        // Apply limit and extract just the summaries
        Ok(results
            .into_iter()
            .take(limit.min(50))
            .map(|(_, s)| s)
            .collect())
    }

    /// Get full session by ID
    pub fn get_session(&self, session_id: &str) -> Result<Option<Session>> {
        let projects_dir = self.base_path.join("projects");

        if !projects_dir.exists() {
            return Ok(None);
        }

        for entry in WalkDir::new(&projects_dir)
            .max_depth(3)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
                // Check if filename matches session_id
                if let Some(stem) = path.file_stem() {
                    if stem.to_string_lossy() == session_id {
                        if let Ok(Some(session)) = self.try_parse_jsonl_session(path) {
                            return Ok(Some(session));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// Get context summary of a session
    pub fn get_session_context(&self, session_id: &str) -> Result<Option<SessionContext>> {
        if let Some(session) = self.get_session(session_id)? {
            let initial_request = session
                .messages
                .iter()
                .find(|m| m.role == "user")
                .map(|m| {
                    let content: String = m.content.chars().take(500).collect();
                    if m.content.len() > 500 {
                        format!("{}...", content)
                    } else {
                        content
                    }
                });

            // Extract file paths mentioned
            let files_mentioned = extract_file_paths(&session);

            // Extract key terms (simple word frequency)
            let key_terms = extract_key_terms(&session);

            return Ok(Some(SessionContext {
                id: session.id,
                cwd: session.cwd,
                initial_request,
                message_count: session.messages.len(),
                files_mentioned,
                key_terms,
            }));
        }
        Ok(None)
    }

    /// Parse a JSONL session file (Claude Code's actual format)
    fn try_parse_jsonl_session(&self, path: &Path) -> Result<Option<Session>> {
        let content = std::fs::read_to_string(path)?;
        let lines: Vec<&str> = content.lines().collect();

        if lines.is_empty() {
            return Ok(None);
        }

        let mut messages = Vec::new();
        let mut session_id: Option<String> = None;
        let mut cwd: Option<String> = None;
        let mut first_timestamp: Option<DateTime<Utc>> = None;
        let mut last_timestamp: Option<DateTime<Utc>> = None;

        for line in lines {
            if line.trim().is_empty() {
                continue;
            }

            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Get session ID from any entry
            if session_id.is_none() {
                session_id = value.get("sessionId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }

            // Get cwd from any entry
            if cwd.is_none() {
                cwd = value.get("cwd")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }

            // Parse timestamp
            let timestamp = value.get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<DateTime<Utc>>().ok());

            if let Some(ts) = timestamp {
                if first_timestamp.is_none() || Some(ts) < first_timestamp {
                    first_timestamp = Some(ts);
                }
                if last_timestamp.is_none() || Some(ts) > last_timestamp {
                    last_timestamp = Some(ts);
                }
            }

            // Only process user and assistant messages
            let msg_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if msg_type != "user" && msg_type != "assistant" {
                continue;
            }

            // Extract role and content from the message field
            if let Some(message) = value.get("message") {
                let role = message.get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or(msg_type)
                    .to_string();

                let content = extract_message_content(message);

                if !content.is_empty() {
                    messages.push(Message {
                        role,
                        content,
                        timestamp,
                    });
                }
            }
        }

        if messages.is_empty() {
            return Ok(None);
        }

        // Use filename as session ID if not found in content
        let id = session_id.unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| uuid_from_path(path))
        });

        Ok(Some(Session {
            id,
            project_path: extract_project_path(path),
            cwd,
            created_at: first_timestamp,
            updated_at: last_timestamp.or_else(|| {
                path.metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(DateTime::from)
            }),
            messages,
            file_path: path.to_path_buf(),
        }))
    }
}

/// Extract content from a message object (handles both string and array content formats)
fn extract_message_content(message: &serde_json::Value) -> String {
    if let Some(content) = message.get("content") {
        // String content
        if let Some(s) = content.as_str() {
            return s.to_string();
        }

        // Array content (assistant messages with tool_use, text blocks, etc.)
        if let Some(arr) = content.as_array() {
            let mut parts = Vec::new();
            for item in arr {
                // Text block: {"type": "text", "text": "..."}
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(text.to_string());
                }
                // Tool use block: extract tool name and input summary
                else if item.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                        let input_summary = item.get("input")
                            .map(|i| {
                                if let Some(fp) = i.get("file_path").and_then(|f| f.as_str()) {
                                    format!(" on {}", fp)
                                } else if let Some(cmd) = i.get("command").and_then(|c| c.as_str()) {
                                    let cmd_preview: String = cmd.chars().take(50).collect();
                                    format!(": {}", cmd_preview)
                                } else {
                                    String::new()
                                }
                            })
                            .unwrap_or_default();
                        parts.push(format!("[Tool: {}{}]", name, input_summary));
                    }
                }
            }
            return parts.join("\n");
        }
    }
    String::new()
}

/// Extract project path from session file path
fn extract_project_path(path: &Path) -> Option<String> {
    let components: Vec<_> = path.components().collect();
    for (i, comp) in components.iter().enumerate() {
        if comp.as_os_str() == "projects" && i + 1 < components.len() {
            let project_hash = components[i + 1].as_os_str().to_string_lossy().to_string();
            // Convert hash back to readable path if it starts with -
            if project_hash.starts_with('-') {
                return Some(project_hash.replace('-', "/"));
            }
            return Some(project_hash);
        }
    }
    None
}

/// Generate a UUID-like string from path for sessions without explicit ID
fn uuid_from_path(path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Convert session to summary
fn session_to_summary(session: &Session) -> SessionSummary {
    let preview = session
        .messages
        .iter()
        .find(|m| m.role == "user")
        .map(|m| {
            let content: String = m.content.chars().take(200).collect();
            if m.content.len() > 200 {
                format!("{}...", content)
            } else {
                content
            }
        })
        .unwrap_or_else(|| "No preview available".to_string());

    SessionSummary {
        id: session.id.clone(),
        project_path: session.project_path.clone(),
        cwd: session.cwd.clone(),
        created_at: session.created_at.map(|dt| dt.to_rfc3339()),
        updated_at: session.updated_at.map(|dt| dt.to_rfc3339()),
        message_count: session.messages.len(),
        preview,
    }
}

/// Extract file paths mentioned in session
fn extract_file_paths(session: &Session) -> Vec<String> {
    use std::collections::HashSet;
    let mut paths = HashSet::new();

    for msg in &session.messages {
        for word in msg.content.split_whitespace() {
            if (word.contains('/') || word.contains('\\'))
                && (word.contains('.') || word.ends_with('/'))
            {
                let cleaned = word.trim_matches(|c: char| {
                    !c.is_alphanumeric() && c != '/' && c != '\\' && c != '.' && c != '_' && c != '-'
                });
                if cleaned.len() > 3 {
                    paths.insert(cleaned.to_string());
                }
            }
        }
    }

    let mut result: Vec<_> = paths.into_iter().collect();
    result.sort();
    result.truncate(20);
    result
}

/// Extract key terms from session (simple word frequency)
fn extract_key_terms(session: &Session) -> Vec<String> {
    use std::collections::HashMap;

    let stop_words: std::collections::HashSet<&str> = [
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "must", "shall", "can", "need", "dare",
        "ought", "used", "to", "of", "in", "for", "on", "with", "at", "by",
        "from", "as", "into", "through", "during", "before", "after", "above",
        "below", "between", "under", "again", "further", "then", "once", "here",
        "there", "when", "where", "why", "how", "all", "each", "few", "more",
        "most", "other", "some", "such", "no", "nor", "not", "only", "own",
        "same", "so", "than", "too", "very", "just", "and", "but", "if", "or",
        "because", "until", "while", "this", "that", "these", "those", "i", "you",
        "he", "she", "it", "we", "they", "what", "which", "who", "whom", "its",
        "his", "her", "their", "my", "your", "our", "tool", "file", "path",
    ]
    .iter()
    .copied()
    .collect();

    let mut word_counts: HashMap<String, usize> = HashMap::new();

    for msg in &session.messages {
        for word in msg.content.split_whitespace() {
            let cleaned: String = word
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase();

            if cleaned.len() > 3 && !stop_words.contains(cleaned.as_str()) {
                *word_counts.entry(cleaned).or_insert(0) += 1;
            }
        }
    }

    let mut sorted: Vec<_> = word_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    sorted.into_iter().take(15).map(|(word, _)| word).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_message_content_string() {
        let message = serde_json::json!({
            "role": "user",
            "content": "Hello world"
        });
        assert_eq!(extract_message_content(&message), "Hello world");
    }

    #[test]
    fn test_extract_message_content_array() {
        let message = serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Here is the result"},
                {"type": "tool_use", "name": "Write", "input": {"file_path": "/test/file.rs"}}
            ]
        });
        let content = extract_message_content(&message);
        assert!(content.contains("Here is the result"));
        assert!(content.contains("[Tool: Write on /test/file.rs]"));
    }

    #[test]
    fn test_extract_project_path() {
        let path = Path::new("/home/user/.claude/projects/-home-user-myproject/session.jsonl");
        let project = extract_project_path(path);
        assert_eq!(project, Some("/home/user/myproject".to_string()));
    }
}
