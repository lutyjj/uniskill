use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::config::{self, SkillEntry, SkillSourceKind};

pub fn assemble_explicit_bundle(
    bundle_name: &str,
    skills: &HashMap<String, SkillEntry>,
    base_dir: &Path,
    source_base_dir: &Path,
) -> Result<PathBuf> {
    let bundle_root = base_dir.join("bundles").join(bundle_name);
    let skills_dir = bundle_root.join("skills");
    if bundle_root.exists() {
        fs::remove_dir_all(&bundle_root).with_context(|| {
            format!("failed to clear bundle cache at {}", bundle_root.display())
        })?;
    }
    fs::create_dir_all(&skills_dir)
        .with_context(|| format!("failed to create bundle cache at {}", skills_dir.display()))?;

    for (skill_name, entry) in skills {
        assemble_skill(skill_name, entry, &skills_dir, base_dir, source_base_dir)?;
    }

    Ok(bundle_root)
}

fn assemble_skill(
    skill_name: &str,
    entry: &SkillEntry,
    skills_dir: &Path,
    cache_dir: &Path,
    source_base_dir: &Path,
) -> Result<()> {
    let dest = skills_dir.join(skill_name);
    match entry.source_kind() {
        SkillSourceKind::Url => {
            fs::create_dir_all(&dest)
                .with_context(|| format!("failed to create skill cache dir for '{skill_name}'"))?;
            let url = entry.url.as_deref().expect("url source checked");
            download_skill(url, &dest.join("SKILL.md"))
        }
        SkillSourceKind::Local => {
            let source = entry.source.as_deref().expect("local source checked");
            let source = config::resolve_source_from(source, source_base_dir);
            copy_skill_dir(skill_name, &source, &dest)
        }
        SkillSourceKind::Git => {
            let repo = entry.repo.as_deref().expect("git source checked");
            let skill_path = entry.path.as_deref().ok_or_else(|| {
                anyhow::anyhow!("git skill '{skill_name}' requires a path inside the repository")
            })?;
            let repo = config::resolve_repo_from(repo, source_base_dir);
            let repo_root = fetch_git_repo(&repo, entry.git_ref.as_deref(), cache_dir)?;
            let source = resolve_repo_path(&repo_root, skill_path)?;
            copy_skill_dir(skill_name, &source, &dest)
        }
        SkillSourceKind::Invalid => Err(anyhow::anyhow!(
            "skill '{}' must declare exactly one source: url, source, or repo",
            skill_name
        )),
    }
}

/// Fetch a git repository into the local cache.
fn fetch_git_repo(repo: &str, git_ref: Option<&str>, base_dir: &Path) -> Result<PathBuf> {
    let repo_url = normalize_repo_url(repo);
    let ref_key = git_ref.unwrap_or("head");
    let repo_dir =
        base_dir
            .join("repos")
            .join(format!("{}-{}", cache_key(&repo_url), cache_key(ref_key)));

    if repo_dir.exists() {
        run_git(&repo_dir, &["fetch", "--quiet", "--prune", "origin"])?;
    } else {
        if let Some(parent) = repo_dir.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create git cache directory at {}",
                    parent.display()
                )
            })?;
        }
        run_command(
            Command::new("git")
                .arg("clone")
                .arg("--quiet")
                .arg("--")
                .arg(&repo_url)
                .arg(&repo_dir),
        )
        .with_context(|| format!("failed to clone git skill repository {repo_url}"))?;
    }

    if let Some(requested_ref) = git_ref {
        run_git(&repo_dir, &["checkout", "--quiet", requested_ref])?;
        let _ = run_git(&repo_dir, &["pull", "--ff-only", "--quiet"]);
    } else {
        let _ = run_git(&repo_dir, &["pull", "--ff-only", "--quiet"]);
    }

    Ok(repo_dir)
}

fn resolve_repo_path(repo_dir: &Path, bundle_path: &str) -> Result<PathBuf> {
    let path = Path::new(bundle_path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(anyhow::anyhow!(
            "skill path must be relative and stay inside the repository: {bundle_path}"
        ));
    }

    Ok(repo_dir.join(path))
}

fn copy_skill_dir(skill_name: &str, source: &Path, dest: &Path) -> Result<()> {
    if !source.is_dir() {
        return Err(anyhow::anyhow!(
            "skill '{}' source directory not found: {}",
            skill_name,
            source.display()
        ));
    }
    if !source.join("SKILL.md").is_file() {
        return Err(anyhow::anyhow!(
            "skill '{}' source has no SKILL.md: {}",
            skill_name,
            source.display()
        ));
    }
    copy_dir_all(source, dest)
}

fn copy_dir_all(source: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)
        .with_context(|| format!("failed to create directory {}", dest.display()))?;
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&source_path, &dest_path)?;
        } else {
            fs::copy(&source_path, &dest_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source_path.display(),
                    dest_path.display()
                )
            })?;
        }
    }

    Ok(())
}

fn normalize_repo_url(repo: &str) -> String {
    if let Some(path) = repo.strip_prefix("gh:") {
        github_ssh_url(path)
    } else if let Some(path) = repo.strip_prefix("github:") {
        github_ssh_url(path)
    } else if is_github_shorthand(repo) {
        github_ssh_url(repo)
    } else {
        repo.to_string()
    }
}

fn github_ssh_url(path: &str) -> String {
    let path = path.strip_suffix(".git").unwrap_or(path);
    format!("git@github.com:{path}.git")
}

fn is_github_shorthand(repo: &str) -> bool {
    let mut parts = repo.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(name), None)
            if !owner.is_empty()
                && !name.is_empty()
                && !repo.contains(':')
                && !repo.starts_with('.')
                && !repo.starts_with('/')
    )
}

fn cache_key(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn run_git(repo_dir: &Path, args: &[&str]) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo_dir);
    for arg in args {
        command.arg(arg);
    }
    run_command(&mut command)
}

fn run_command(command: &mut Command) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("failed to start {:?}", command))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(anyhow::anyhow!(
        "command {:?} failed: {}{}",
        command,
        stderr.trim(),
        stdout.trim()
    ))
}

/// Download a single skill URL to the destination path.
/// Skips if the file exists and has the same size as the remote content.
fn download_skill(url: &str, dest: &Path) -> Result<()> {
    // Check existing file first — fast path for unchanged skills.
    let existing_size = fs::metadata(dest).ok().map(|m| m.len());

    let response = ureq::get(url)
        .call()
        .with_context(|| format!("failed to fetch skill from {url}"))?;

    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(anyhow::anyhow!(
            "HTTP {} when fetching skill from {url}",
            status
        ));
    }

    let body: String = response
        .into_string()
        .with_context(|| format!("failed to read response body from {url}"))?;

    let body_bytes = body.as_bytes();

    // Skip download if file exists with identical size and content.
    if let Some(existing) = existing_size {
        if existing as usize == body_bytes.len() {
            let existing_content = fs::read(dest).ok();
            if let Some(ref existing_bytes) = existing_content {
                if *existing_bytes == body_bytes {
                    return Ok(());
                }
            }
        }
    }

    // Write atomically: write to temp file, then rename.
    let tmp = dest.with_extension("tmp");
    let mut file =
        File::create(&tmp).with_context(|| format!("failed to create temp file at {:?}", tmp))?;
    file.write_all(body_bytes)
        .with_context(|| format!("failed to write skill content to {:?}", dest))?;
    drop(file);

    // Atomic rename replaces the old file.
    fs::rename(&tmp, dest)
        .with_context(|| format!("failed to replace skill file at {:?}", dest))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_assemble_explicit_bundle_copies_local_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("cache");
        let source = tmp.path().join("source").join("code-design");
        fs::create_dir_all(source.join("agents")).unwrap();
        fs::write(source.join("SKILL.md"), "---\nname: code-design\n---\n").unwrap();
        fs::write(source.join("agents").join("openai.yaml"), "version: 1\n").unwrap();

        let mut skills = HashMap::new();
        skills.insert(
            "code-design".to_string(),
            SkillEntry {
                source: Some(source.to_string_lossy().to_string()),
                ..SkillEntry::default()
            },
        );

        let bundle_root = assemble_explicit_bundle("generic", &skills, &base, tmp.path()).unwrap();

        assert_eq!(bundle_root, base.join("bundles").join("generic"));
        assert!(bundle_root
            .join("skills")
            .join("code-design")
            .join("SKILL.md")
            .exists());
        assert!(bundle_root
            .join("skills")
            .join("code-design")
            .join("agents")
            .join("openai.yaml")
            .exists());
    }

    #[test]
    fn test_assemble_explicit_bundle_downloads_url_skill() {
        let Some(url) = serve_once("# Remote Skill\n\nTest content") else {
            return;
        };

        let tmp = tempfile::tempdir().unwrap();
        let mut skills = HashMap::new();
        skills.insert(
            "remote-skill".to_string(),
            SkillEntry {
                url: Some(url),
                ..SkillEntry::default()
            },
        );

        let bundle_root =
            assemble_explicit_bundle("remote", &skills, &tmp.path().join("cache"), tmp.path())
                .unwrap();

        let content = fs::read_to_string(
            bundle_root
                .join("skills")
                .join("remote-skill")
                .join("SKILL.md"),
        )
        .unwrap();
        assert_eq!(content, "# Remote Skill\n\nTest content");
    }

    #[test]
    fn test_download_skill_writes_content() {
        let expected_body = "# My Skill\n\nTest content";
        let Some(url) = serve_once(expected_body) else {
            return;
        };

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("SKILL.md");

        let result = download_skill(&url, &dest);
        assert!(result.is_ok());

        let content = fs::read_to_string(&dest).unwrap();
        assert_eq!(content, expected_body);
    }

    #[test]
    fn test_normalize_repo_url_supports_github_shorthand() {
        assert_eq!(
            normalize_repo_url("lutyjj/agent-skills"),
            "git@github.com:lutyjj/agent-skills.git"
        );
        assert_eq!(
            normalize_repo_url("gh:lutyjj/agent-skills"),
            "git@github.com:lutyjj/agent-skills.git"
        );
        assert_eq!(
            normalize_repo_url("github:lutyjj/agent-skills.git"),
            "git@github.com:lutyjj/agent-skills.git"
        );
        assert_eq!(
            normalize_repo_url("https://github.com/lutyjj/agent-skills.git"),
            "https://github.com/lutyjj/agent-skills.git"
        );
    }

    #[test]
    fn test_resolve_repo_path_rejects_escape() {
        let repo = Path::new("/tmp/repo");
        assert!(resolve_repo_path(repo, "../outside").is_err());
        assert!(resolve_repo_path(repo, "/outside").is_err());
        assert_eq!(
            resolve_repo_path(repo, "bundles/generic/skills/code-design").unwrap(),
            PathBuf::from("/tmp/repo/bundles/generic/skills/code-design")
        );
    }

    #[test]
    fn test_assemble_explicit_bundle_uses_git_skill_path() {
        let tmp = tempfile::tempdir().unwrap();
        let source_repo = tmp.path().join("source");
        fs::create_dir_all(
            source_repo
                .join("bundles")
                .join("generic")
                .join("skills")
                .join("code-design"),
        )
        .unwrap();
        fs::write(
            source_repo
                .join("bundles")
                .join("generic")
                .join("skills")
                .join("code-design")
                .join("SKILL.md"),
            "---\nname: code-design\n---\n",
        )
        .unwrap();

        run_command(Command::new("git").arg("init").arg(&source_repo)).unwrap();
        run_git(&source_repo, &["config", "user.email", "test@example.com"]).unwrap();
        run_git(&source_repo, &["config", "user.name", "Test User"]).unwrap();
        run_git(&source_repo, &["add", "."]).unwrap();
        run_git(&source_repo, &["commit", "--quiet", "-m", "init"]).unwrap();

        let mut skills = HashMap::new();
        skills.insert(
            "code-design".to_string(),
            SkillEntry {
                repo: Some(source_repo.to_string_lossy().to_string()),
                path: Some("bundles/generic/skills/code-design".to_string()),
                ..SkillEntry::default()
            },
        );

        let bundle_root =
            assemble_explicit_bundle("generic", &skills, &tmp.path().join("cache"), tmp.path())
                .unwrap();

        assert!(bundle_root
            .join("skills")
            .join("code-design")
            .join("SKILL.md")
            .exists());
    }

    #[test]
    fn test_assemble_explicit_bundle_rejects_invalid_skill_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let mut skills = HashMap::new();
        skills.insert(
            "bad".to_string(),
            SkillEntry {
                url: Some("https://example.com/skill.md".to_string()),
                source: Some("/tmp/skill".to_string()),
                ..SkillEntry::default()
            },
        );

        let result =
            assemble_explicit_bundle("generic", &skills, &tmp.path().join("cache"), tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_assemble_explicit_bundle_rejects_git_skill_without_path() {
        let tmp = tempfile::tempdir().unwrap();
        let mut skills = HashMap::new();
        skills.insert(
            "bad".to_string(),
            SkillEntry {
                repo: Some("gh:lutyjj/agent-skills".to_string()),
                ..SkillEntry::default()
            },
        );

        let result =
            assemble_explicit_bundle("generic", &skills, &tmp.path().join("cache"), tmp.path());
        assert!(result.is_err());
        assert!(!tmp.path().join("cache").join("repos").exists());
    }

    fn serve_once(body: &'static str) -> Option<String> {
        use tiny_http::{Response, Server};

        let server = match Server::http("127.0.0.1:0") {
            Ok(server) => server,
            Err(err)
                if err
                    .downcast_ref::<std::io::Error>()
                    .map(|err| err.kind() == std::io::ErrorKind::PermissionDenied)
                    .unwrap_or(false) =>
            {
                return None
            }
            Err(err) => panic!("failed to start local test HTTP server: {err}"),
        };
        let port = match server.server_addr() {
            tiny_http::ListenAddr::IP(sa) => sa.port(),
            #[cfg(unix)]
            tiny_http::ListenAddr::Unix(_) => unreachable!(),
        };

        std::thread::spawn(move || {
            if let Ok(request) = server.recv() {
                let response = Response::from_string(body);
                request.respond(response).ok();
            }
        });

        Some(format!("http://127.0.0.1:{port}/skill"))
    }
}
