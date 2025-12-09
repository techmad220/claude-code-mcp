//! Claude Code session history parser
//!
//! Claude Code stores sessions in ~/.claude/ with structure:
//! - projects/<project-hash>/sessions/<session-id>.json
//! - Or potentially in other locations depending on version

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
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub message_count: usize,
    pub preview: String,
}

/// Context summary of a session
#[derive(Debug, Serialize)]
pub struct SessionContext {
    pub id: String,
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

        // Walk through the claude directory looking for session files
        for entry in WalkDir::new(&self.base_path)
            .max_depth(5)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "json") {
                if let Ok(Some(session)) = self.try_parse_session(path) {
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

        for entry in WalkDir::new(&self.base_path)
            .max_depth(5)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "json") {
                if let Ok(Some(session)) = self.try_parse_session(path) {
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
        for entry in WalkDir::new(&self.base_path)
            .max_depth(5)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "json") {
                if let Ok(Some(session)) = self.try_parse_session(path) {
                    if session.id == session_id {
                        return Ok(Some(session));
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
                .find(|m| m.role == "user" || m.role == "human")
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
                initial_request,
                message_count: session.messages.len(),
                files_mentioned,
                key_terms,
            }));
        }
        Ok(None)
    }

    /// Try to parse a file as a session
    fn try_parse_session(&self, path: &Path) -> Result<Option<Session>> {
        let content = std::fs::read_to_string(path)?;

        // Try parsing as single JSON object
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(session) = parse_session_value(&value, path) {
                return Ok(Some(session));
            }
        }

        // Try parsing as JSONL (one message per line)
        let lines: Vec<&str> = content.lines().collect();
        if !lines.is_empty() {
            let mut messages = Vec::new();
            for line in lines {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                    let role = value
                        .get("role")
                        .and_then(|r| r.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let content = extract_content(&value);
                    if !content.is_empty() {
                        messages.push(Message {
                            role,
                            content,
                            timestamp: None,
                        });
                    }
                }
            }
            if !messages.is_empty() {
                let id = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| uuid_from_path(path));

                return Ok(Some(Session {
                    id,
                    project_path: extract_project_path(path),
                    created_at: None,
                    updated_at: path
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(DateTime::from),
                    messages,
                    file_path: path.to_path_buf(),
                }));
            }
        }
        Ok(None)
    }
}

/// Parse a JSON value as a session
fn parse_session_value(value: &serde_json::Value, path: &Path) -> Option<Session> {
    // Look for messages array
    let messages_value = value.get("messages").or_else(|| value.get("conversation"))?;
    let messages_arr = messages_value.as_array()?;

    let messages: Vec<Message> = messages_arr
        .iter()
        .filter_map(|m| {
            let role = m
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("unknown")
                .to_string();
            let content = extract_content(m);
            if content.is_empty() {
                None
            } else {
                Some(Message {
                    role,
                    content,
                    timestamp: m
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .and_then(|s| s.parse().ok()),
                })
            }
        })
        .collect();

    if messages.is_empty() {
        return None;
    }

    let id = value
        .get("id")
        .or_else(|| value.get("session_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| uuid_from_path(path))
        });

    Some(Session {
        id,
        project_path: extract_project_path(path),
        created_at: value
            .get("created_at")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok()),
        updated_at: value
            .get("updated_at")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .or_else(|| {
                path.metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(DateTime::from)
            }),
        messages,
        file_path: path.to_path_buf(),
    })
}

/// Extract content from a message value (handles both string and array formats)
fn extract_content(value: &serde_json::Value) -> String {
    if let Some(content) = value.get("content") {
        if let Some(s) = content.as_str() {
            return s.to_string();
        }
        if let Some(arr) = content.as_array() {
            return arr
                .iter()
                .filter_map(|item| {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        Some(text.to_string())
                    } else {
                        item.as_str().map(|s| s.to_string())
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
    String::new()
}

/// Extract project path from session file path
fn extract_project_path(path: &Path) -> Option<String> {
    // Try to find "projects" in the path and get the project name
    let components: Vec<_> = path.components().collect();
    for (i, comp) in components.iter().enumerate() {
        if comp.as_os_str() == "projects" && i + 1 < components.len() {
            return Some(components[i + 1].as_os_str().to_string_lossy().to_string());
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
        .find(|m| m.role == "user" || m.role == "human")
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

    // Simple regex-like matching for file paths
    for msg in &session.messages {
        for word in msg.content.split_whitespace() {
            // Look for things that look like file paths
            if (word.contains('/') || word.contains('\\'))
                && (word.contains('.') || word.ends_with('/'))
            {
                let cleaned = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '\\' && c != '.' && c != '_' && c != '-');
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
        "his", "her", "their", "my", "your", "our",
    ].iter().copied().collect();

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
    fn test_extract_content_string() {
        let value = serde_json::json!({
            "role": "user",
            "content": "Hello world"
        });
        assert_eq!(extract_content(&value), "Hello world");
    }

    #[test]
    fn test_extract_content_array() {
        let value = serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Hello"},
                {"type": "text", "text": "World"}
            ]
        });
        assert_eq!(extract_content(&value), "Hello\nWorld");
    }
}
