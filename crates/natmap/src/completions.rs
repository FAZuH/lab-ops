use std::ffi::OsStr;
use std::process::Command;

use clap_complete::engine::CompletionCandidate;

pub fn complete_container_id(current: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(output) = Command::new("docker")
        .args(["ps", "--format", "{{.Names}}\t{{.ID}}"])
        .output()
    else {
        return vec![];
    };

    if !output.status.success() {
        return vec![];
    }

    let Some(current_str) = current.to_str() else {
        return vec![];
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            let (name, id) = line.split_once('\t')?;
            let short_id: String = id.chars().take(12).collect();
            Some((name.to_string(), short_id))
        })
        .filter(|(name, id)| name.starts_with(current_str) || id.starts_with(current_str))
        .flat_map(|(name, id)| {
            vec![
                CompletionCandidate::new(&name).help(Some(id.clone().into())),
                CompletionCandidate::new(id).help(Some(name.into())),
            ]
        })
        .collect()
}
