use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DotenvFile {
    path: PathBuf,
    lines: Vec<DotenvLine>,
}

#[derive(Debug, Clone)]
enum DotenvLine {
    Comment(String),
    Empty,
    Variable {
        key: String,
        value: String,
        raw: String,
    },
}

impl DotenvFile {
    pub fn read(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self {
                path: path.to_path_buf(),
                lines: Vec::new(),
            });
        }

        let contents = fs::read_to_string(path)
            .map_err(|err| format!("Failed to read dotenv file {}: {err}", path.display()))?;
        let mut seen_keys = HashSet::new();
        let mut lines = Vec::new();

        for (index, line) in contents.lines().enumerate() {
            let parsed = parse_line(line).map_err(|err| {
                format!(
                    "Failed to parse dotenv file {} at line {}: {err}",
                    path.display(),
                    index + 1
                )
            })?;

            if let DotenvLine::Variable { key, .. } = &parsed {
                if !seen_keys.insert(key.clone()) {
                    return Err(format!(
                        "Duplicate variable '{key}' in dotenv file {}",
                        path.display()
                    ));
                }
            }

            lines.push(parsed);
        }

        Ok(Self {
            path: path.to_path_buf(),
            lines,
        })
    }

    pub fn get_vars(&self) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        for line in &self.lines {
            if let DotenvLine::Variable { key, value, .. } = line {
                vars.insert(key.clone(), value.clone());
            }
        }
        vars
    }

    pub fn update(&mut self, key: &str, value: &str) -> Result<(), String> {
        if !is_valid_key(key) {
            return Err(format!("Invalid dotenv variable name '{key}'"));
        }

        let mut updated = false;
        for line in &mut self.lines {
            if let DotenvLine::Variable {
                key: existing,
                value: existing_value,
                raw,
            } = line
            {
                if existing == key {
                    *existing_value = value.to_string();
                    *raw = format!("{key}={value}");
                    updated = true;
                    break;
                }
            }
        }

        if !updated {
            self.lines.push(DotenvLine::Variable {
                key: key.to_string(),
                value: value.to_string(),
                raw: format!("{key}={value}"),
            });
        }

        self.write()
    }

    fn write(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|err| {
                    format!(
                        "Failed to create dotenv directory {}: {err}",
                        parent.display()
                    )
                })?;
            }
        }

        let contents = self
            .lines
            .iter()
            .map(|line| match line {
                DotenvLine::Comment(raw) => raw.as_str(),
                DotenvLine::Empty => "",
                DotenvLine::Variable { raw, .. } => raw.as_str(),
            })
            .collect::<Vec<&str>>()
            .join("\n");

        fs::write(&self.path, contents)
            .map_err(|err| format!("Failed to write dotenv file {}: {err}", self.path.display()))
    }
}

fn parse_line(line: &str) -> Result<DotenvLine, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(DotenvLine::Empty);
    }
    if trimmed.starts_with('#') {
        return Ok(DotenvLine::Comment(line.to_string()));
    }

    let (export_stripped, has_export) = if let Some(stripped) = trimmed.strip_prefix("export ") {
        (stripped, true)
    } else {
        (trimmed, false)
    };

    let eq_index = export_stripped.find('=').ok_or_else(|| {
        if has_export {
            "Invalid dotenv line after export prefix".to_string()
        } else {
            "Invalid dotenv line, missing '='".to_string()
        }
    })?;

    if eq_index == 0 {
        return Err("Invalid dotenv line, missing key".to_string());
    }

    let before = export_stripped[..eq_index].chars().last();
    let after = export_stripped[eq_index + 1..].chars().next();
    if before.is_some_and(|ch| ch.is_whitespace()) || after.is_some_and(|ch| ch.is_whitespace()) {
        return Err("Whitespace around '=' is not allowed".to_string());
    }

    let key = &export_stripped[..eq_index];
    if !is_valid_key(key) {
        return Err(format!("Invalid dotenv variable name '{key}'"));
    }

    let mut value = export_stripped[eq_index + 1..].to_string();
    if value.starts_with('"') || value.starts_with('\'') {
        let quote = match value.chars().next() {
            Some(q) => q,
            None => return Err("Invalid empty quoted value".to_string()),
        };
        if !value.ends_with(quote) || value.len() == 1 {
            return Err("Invalid quoted value".to_string());
        }
        value = value[1..value.len() - 1].to_string();
    }

    Ok(DotenvLine::Variable {
        key: key.to_string(),
        value,
        raw: line.to_string(),
    })
}

fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}
