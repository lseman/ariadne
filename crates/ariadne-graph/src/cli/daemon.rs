//! cmd_daemon, cmd_install, install_agents_md, install_mcp_config.

use anyhow::{bail, Result};
use serde_json::json;
use std::path::{Path, PathBuf};

use super::handlers::DaemonCommands;
use super::watch;

pub fn cmd_daemon(db: &Path, command: DaemonCommands) -> Result<()> {
    use super::git::{load_daemon_repos, save_daemon_repos};

    match command {
        DaemonCommands::Add { path, alias } => {
            let mut repos = load_daemon_repos()?;
            let path = super::git::absolute_path(&path)?;
            let alias = alias.unwrap_or_else(|| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("repo")
                    .to_string()
            });
            repos.push(json!({ "alias": alias, "path": path }));
            save_daemon_repos(&repos)?;
            println!("registered {}", path.display());
            Ok(())
        }
        DaemonCommands::Status => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "repos": load_daemon_repos()? }))?
            );
            Ok(())
        }
        DaemonCommands::Start { interval } => {
            let repos = load_daemon_repos()?;
            if repos.is_empty() {
                bail!("no repositories registered; run ariadne daemon add <path>");
            }
            let roots: Vec<PathBuf> = repos
                .iter()
                .filter_map(|repo| repo["path"].as_str().map(PathBuf::from))
                .collect();
            println!("Ariadne daemon watching {} repos", roots.len());
            let err = match watch::watch_event_driven(db, &roots) {
                Err(e) => e,
                Ok(never) => match never {},
            };
            tracing::warn!(
                "file watcher unavailable ({}); falling back to polling every {}s",
                err,
                interval
            );
            loop {
                for root in &roots {
                    if let Err(e) = super::build::cmd_update(db, root) {
                        tracing::warn!("daemon update failed for {}: {}", root.display(), e);
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(interval.max(1)));
            }
        }
    }
}

/// Install auto-update git hooks for this repository.
pub fn cmd_install(db: &Path, repo: &Path, force: bool, agents: bool, mcp: bool) -> Result<()> {
    let git_dir = repo.join(".git");
    if !git_dir.is_dir() {
        bail!("{} is not a git repository", repo.display());
    }
    let hooks_dir = git_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let exe = std::env::current_exe()?;
    let db = super::git::absolute_path(db)?;
    let root = super::git::absolute_path(repo)?;

    for hook in ["post-commit", "post-merge", "post-checkout"] {
        let path = hooks_dir.join(hook);
        if path.exists() && !force {
            bail!(
                "{} already exists; rerun with --force to replace it",
                path.display()
            );
        }
        let script = format!(
            "#!/bin/sh\n\"{}\" --db \"{}\" update \"{}\" >/dev/null 2>&1 || true\n",
            exe.display(),
            db.display(),
            root.display()
        );
        std::fs::write(&path, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
    }

    println!(
        "installed Ariadne auto-update hooks in {}",
        hooks_dir.display()
    );
    if agents {
        install_agents_md(repo, &db)?;
    }
    if mcp {
        install_mcp_config(repo, &db)?;
    }
    Ok(())
}

fn install_agents_md(repo: &Path, db: &Path) -> Result<()> {
    let path = repo.join("AGENTS.md");
    let block = format!(
        r#"# Ariadne Agent Instructions

- Start exploration with `ariadne --db {} tool minimal_context --params '{{"target":"...","mode":"review"}}'`.
- For code review, run `ariadne --db {} tool detect_changes --params '{{"base":"HEAD~1"}}'` before reading files.
- Use `impact`, `traverse`, and `review_context` to gather bounded context before broad grep/read.
- Use `gaps`, `bridge_nodes`, and `large_functions` to find risky areas and review questions.
- Fall back to direct file reads only after Ariadne identifies the relevant files or symbols.
"#,
        db.display(),
        db.display()
    );
    std::fs::write(&path, block)?;
    println!("installed {}", path.display());
    Ok(())
}

fn install_mcp_config(repo: &Path, db: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let claude_path = repo.join(".mcp.json");
    std::fs::write(
        &claude_path,
        serde_json::to_string_pretty(&super::mcp::mcp_servers_config(&exe, db))?,
    )?;
    println!("installed {}", claude_path.display());

    let cursor_dir = repo.join(".cursor");
    std::fs::create_dir_all(&cursor_dir)?;
    let cursor_path = cursor_dir.join("mcp.json");
    std::fs::write(
        &cursor_path,
        serde_json::to_string_pretty(&super::mcp::mcp_servers_config(&exe, db))?,
    )?;
    println!("installed {}", cursor_path.display());

    let vscode_dir = repo.join(".vscode");
    std::fs::create_dir_all(&vscode_dir)?;
    let vscode_path = vscode_dir.join("mcp.json");
    std::fs::write(
        &vscode_path,
        serde_json::to_string_pretty(&super::mcp::vscode_mcp_config(&exe, db))?,
    )?;
    println!("installed {}", vscode_path.display());

    let codex_dir = repo.join(".codex");
    std::fs::create_dir_all(&codex_dir)?;
    let codex_path = codex_dir.join("ariadne-mcp.toml");
    std::fs::write(&codex_path, super::mcp::codex_mcp_toml(&exe, db))?;
    println!("installed {}", codex_path.display());
    Ok(())
}
