use super::*;
use std::path::{Component, Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub description: String,
    pub path: String,
    pub kind: String,
}

fn split(text: &str, delimiter: char) -> Vec<String> {
    let mut result = vec![String::new()];
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            if c != delimiter && c != '\\' {
                result.last_mut().unwrap().push('\\');
            }
            result.last_mut().unwrap().push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == delimiter {
            result.push(String::new());
        } else {
            result.last_mut().unwrap().push(c);
        }
    }
    if escaped {
        result.last_mut().unwrap().push('\\');
    }
    result
}

impl Attachment {
    pub fn parse(value: &str) -> Result<Vec<Self>> {
        split(value,';').into_iter().filter(|s|!s.trim().is_empty()).map(|part| {
            let fields=split(&part,':');
            match fields.as_slice() {
                [path] =>Ok(Self {description:String::new(),path:path.clone(),kind:"PDF".into()}),
                [description,path,kind]=>Ok(Self {description:description.clone(),path:path.clone(),kind:kind.clone()}),
                _=>Err(invalid("unsupported attachment path; use description:path:type with escaped colons")),
            }
        }).collect()
    }
    pub fn format(files: &[Self]) -> String {
        let escape = |s: &str| {
            s.replace('\\', "\\\\")
                .replace(':', "\\:")
                .replace(';', "\\;")
        };
        files
            .iter()
            .map(|f| {
                format!(
                    "{}:{}:{}",
                    escape(&f.description),
                    escape(&f.path),
                    escape(&f.kind)
                )
            })
            .collect::<Vec<_>>()
            .join(";")
    }
}

/// Resolve a relative file link against the bibliography, without permitting a vault escape.
pub fn attachment_path(bib: &str, file: &str) -> Result<String> {
    if Path::new(file).is_absolute() || file.contains("://") || file.contains('\\') {
        return Err(invalid(
            "attachment must be a relative path inside the vault",
        ));
    }
    let joined = Path::new(bib).parent().unwrap_or(Path::new("")).join(file);
    let mut parts = Vec::new();
    for c in joined.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(invalid("attachment is outside the vault"));
                }
            }
            _ => return Err(invalid("attachment is outside the vault")),
        }
    }
    if parts.is_empty() {
        return Err(invalid("attachment has no filename"));
    }
    Ok(parts.join("/"))
}

pub fn rebase_files(from: &str, to: &str, files: &[Attachment]) -> Result<Vec<Attachment>> {
    let normalized = attachment_path("", to)?;
    let to = Path::new(&normalized)
        .parent()
        .unwrap_or(Path::new(""))
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    files
        .iter()
        .map(|f| {
            let absolute = attachment_path(from, &f.path)?;
            let parts = absolute.split('/').collect::<Vec<_>>();
            let common = to
                .iter()
                .zip(&parts)
                .take_while(|(a, b)| a.as_str() == **b)
                .count();
            let path = std::iter::repeat_n("..", to.len() - common)
                .chain(parts[common..].iter().copied())
                .collect::<Vec<_>>()
                .join("/");
            Ok(Attachment { path, ..f.clone() })
        })
        .collect()
}
