use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::config::{Bundle, GitSource, Source, SourceSpec};

/// Assemble a bundle into `cache_dir/bundles/<name>/skills/`.
///
/// Two composable layers land in the same `skills/` directory:
/// 1. the whole-bundle `source`, whose own `skills/*` are placed in as a unit;
/// 2. explicit per-skill entries, which add to or override those by name.
///
/// A local `source` is placed by symlink when `bundle.link` (the default), so
/// edits flow both ways between harness and working tree; remote `repo` and
/// `url` sources are always copied — they have no working tree to link.
#[cfg(test)]
pub fn assemble_bundle(
    bundle_name: &str,
    bundle: &Bundle,
    cache_dir: &Path,
    source_base_dir: &Path,
) -> Result<PathBuf> {
    let mut bundle = bundle.clone();
    crate::config::anchor_bundle_paths(&mut bundle, source_base_dir);
    let bundles = std::collections::HashMap::from([(bundle_name.into(), bundle.clone())]);
    let prepared = crate::fetch_plan::prepare(&bundles, cache_dir, 4, &mut |_| {});
    stage_bundle(bundle_name, &bundle, cache_dir, &prepared)?.publish()
}

pub struct StagedBundle {
    pub paths: crate::cache::BundlePaths,
}

impl StagedBundle {
    pub fn publish(self) -> Result<PathBuf> {
        replace_bundle_cache(&self.paths)?;
        Ok(self.paths.published.clone())
    }
}

impl Drop for StagedBundle {
    fn drop(&mut self) {
        let _ = remove_existing(&self.paths.staging);
    }
}

pub fn stage_bundle(
    bundle_name: &str,
    bundle: &Bundle,
    cache_dir: &Path,
    prepared: &crate::fetch_plan::Prepared,
) -> Result<StagedBundle> {
    validate_bundle(bundle)?;
    let paths = crate::cache::Layout::resolve(cache_dir)?.bundle(bundle_name)?;
    recover_bundle_cache(&paths)?;
    let skills_dir = paths.staging.join("skills");
    remove_existing(&paths.staging)?;
    fs::create_dir_all(&skills_dir)
        .with_context(|| format!("failed to create bundle cache at {}", skills_dir.display()))?;

    if let Some(source) = bundle
        .source
        .resolve()
        .map_err(|e| anyhow::anyhow!("bundle '{bundle_name}' {e}"))?
    {
        // Only a local working tree can be linked live; remote sources copy.
        let link = bundle.link && matches!(source, Source::Local(_));
        let bundle_dir = materialize_bundle_dir(bundle_name, &source, prepared)?;
        place_bundle_skills(bundle_name, &bundle_dir, &skills_dir, link)?;
    }

    let mut skills: Vec<_> = bundle.skills.iter().collect();
    skills.sort_by_key(|(name, _)| *name);
    for (skill_name, spec) in skills {
        let source = resolve_skill_source(skill_name, spec)?;
        let dest = skills_dir.join(skill_name);
        // An explicit skill overrides whatever the whole-bundle layer placed
        // under the same name — clear it first so link/copy start clean.
        remove_existing(&dest)?;
        materialize_skill(skill_name, &source, &dest, bundle.link, prepared)?;
    }

    Ok(StagedBundle { paths })
}

pub(crate) fn validate_bundle(bundle: &Bundle) -> Result<()> {
    if bundle.source.is_empty() && bundle.skills.is_empty() {
        anyhow::bail!("bundle requires a source or explicit skills");
    }
    if matches!(bundle.source.resolve()?, Some(Source::Url(_))) {
        anyhow::bail!("a whole-bundle source cannot be a URL");
    }
    for spec in std::iter::once(&bundle.source).chain(bundle.skills.values()) {
        if let Some(Source::Git {
            path: Some(path), ..
        }) = spec.resolve()?
        {
            resolve_repo_path(Path::new("/"), &path)?;
        }
    }
    for (name, spec) in &bundle.skills {
        crate::config::validate_component(name)?;
        resolve_skill_source(name, spec)?;
    }
    Ok(())
}

/// Remove a path whether it is a symlink, file, or directory. A no-op if absent.
fn remove_existing(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => {
            fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
        }
        Ok(_) => {
            fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
        }
        Err(_) => Ok(()),
    }
}

fn same_tree(left: &Path, right: &Path) -> Result<bool> {
    let a = fs::symlink_metadata(left)?;
    let b = match fs::symlink_metadata(right) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if a.file_type() != b.file_type() || a.permissions().mode() != b.permissions().mode() {
        return Ok(false);
    }
    if a.file_type().is_symlink() {
        return Ok(fs::read_link(left)? == fs::read_link(right)?);
    }
    if a.is_file() {
        return Ok(a.len() == b.len() && fs::read(left)? == fs::read(right)?);
    }
    if !a.is_dir() {
        anyhow::bail!("unsupported file type in bundle cache");
    }
    let names = |path| -> Result<std::collections::BTreeSet<_>> {
        fs::read_dir(path)?
            .map(|entry| Ok(entry?.file_name()))
            .collect()
    };
    let children = names(left)?;
    if children != names(right)? {
        return Ok(false);
    }
    for name in children {
        if !same_tree(&left.join(&name), &right.join(&name))? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn replace_bundle_cache(paths: &crate::cache::BundlePaths) -> Result<()> {
    let staging_root = &paths.staging;
    let bundle_root = &paths.published;
    let backup_root = &paths.previous;
    recover_bundle_cache(paths)?;
    if same_tree(staging_root, bundle_root)? {
        return remove_existing(staging_root);
    }
    remove_existing(backup_root)?;

    let had_existing = fs::symlink_metadata(bundle_root).is_ok();
    if had_existing {
        fs::rename(bundle_root, backup_root).with_context(|| {
            format!(
                "failed to move existing bundle cache {} aside",
                bundle_root.display()
            )
        })?;
    }

    match fs::rename(staging_root, bundle_root) {
        Ok(()) => {
            remove_existing(backup_root)?;
            Ok(())
        }
        Err(err) => {
            if had_existing {
                let _ = fs::rename(backup_root, bundle_root);
            }
            Err(err).with_context(|| {
                format!(
                    "failed to promote staged bundle cache {} to {}",
                    staging_root.display(),
                    bundle_root.display()
                )
            })
        }
    }
}

/// Restore the published path before source checks or worktree readers use it.
/// The caller holds the cache lock; a remaining staging tree is disposable.
pub(crate) fn recover_bundle_cache(paths: &crate::cache::BundlePaths) -> Result<()> {
    let bundle_root = &paths.published;
    let backup = &paths.previous;
    match fs::symlink_metadata(bundle_root) {
        Ok(_) => remove_existing(backup),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::symlink_metadata(backup) {
                Ok(_) => fs::rename(backup, bundle_root).context("failed to restore bundle cache"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Resolve a skill's source spec, requiring exactly one source to be declared.
fn resolve_skill_source(skill_name: &str, spec: &SourceSpec) -> Result<Source> {
    spec.resolve()
        .map_err(|e| anyhow::anyhow!("skill '{skill_name}' {e}"))?
        .ok_or_else(|| anyhow::anyhow!("skill '{skill_name}' declares no source"))
}

/// Resolve a whole-bundle source to a local bundle directory (containing
/// `skills/`). A `url` source is rejected — a url is a single file, not a bundle.
fn materialize_bundle_dir(
    bundle_name: &str,
    source: &Source,
    prepared: &crate::fetch_plan::Prepared,
) -> Result<PathBuf> {
    match source {
        Source::Local(path) => Ok(PathBuf::from(path)),
        Source::Git {
            repo,
            git_ref,
            path,
        } => resolve_git_dir(repo, git_ref.as_deref(), path.as_deref(), prepared),
        Source::Url(_) => Err(anyhow::anyhow!(
            "bundle '{bundle_name}' cannot use a `url` source — a url is a single file, \
             not a bundle; declare it as a per-skill entry under \
             [bundles.{bundle_name}.skills.<name>] instead"
        )),
    }
}

/// Place every skill directory (one containing `SKILL.md`) from a bundle
/// source's `skills/` folder into the assembled bundle, by symlink when `link`.
fn place_bundle_skills(
    bundle_name: &str,
    bundle_dir: &Path,
    dest_skills_dir: &Path,
    link: bool,
) -> Result<()> {
    let src_skills = bundle_dir.join("skills");
    if !src_skills.is_dir() {
        return Err(anyhow::anyhow!(
            "bundle '{bundle_name}' source has no skills/ directory at {}",
            bundle_dir.display()
        ));
    }

    let mut placed = 0;
    for entry in fs::read_dir(&src_skills)
        .with_context(|| format!("failed to read {}", src_skills.display()))?
    {
        let entry = entry?;
        let skill_path = entry.path();
        if !crate::skill::is_skill_dir(&skill_path) {
            continue;
        }
        let name = entry.file_name();
        let dest = dest_skills_dir.join(&name);
        let skill_name = name.to_string_lossy();
        if link {
            link_skill_dir(&skill_name, &skill_path, &dest)?;
        } else {
            copy_dir_all(&skill_path, &dest)?;
        }
        placed += 1;
    }

    if placed == 0 {
        return Err(anyhow::anyhow!(
            "bundle '{bundle_name}' source has no skills under {}",
            src_skills.display()
        ));
    }
    Ok(())
}

/// Materialize a single skill into `dest` (a directory containing `SKILL.md`).
/// A local source is symlinked when `link`; remote and url sources are copied.
fn materialize_skill(
    skill_name: &str,
    source: &Source,
    dest: &Path,
    link: bool,
    prepared: &crate::fetch_plan::Prepared,
) -> Result<()> {
    match source {
        Source::Local(path) => {
            let source = PathBuf::from(path);
            if link {
                link_skill_dir(skill_name, &source, dest)
            } else {
                copy_skill_dir(skill_name, &source, dest)
            }
        }
        Source::Git {
            repo,
            git_ref,
            path,
        } => {
            let source = resolve_git_dir(repo, git_ref.as_deref(), path.as_deref(), prepared)?;
            copy_skill_dir(skill_name, &source, dest)
        }
        Source::Url(url) => {
            fs::create_dir_all(dest)
                .with_context(|| format!("failed to create skill cache dir for '{skill_name}'"))?;
            fs::copy(prepared.url(url)?, dest.join("SKILL.md"))
                .map(|_| ())
                .context("failed to copy downloaded skill")
        }
    }
}

/// Fetch a git repo into the cache and resolve `path` within it (repo root when
/// `path` is absent). Shared by whole-bundle and per-skill git sources.
fn resolve_git_dir(
    repo: &str,
    git_ref: Option<&str>,
    path: Option<&str>,
    prepared: &crate::fetch_plan::Prepared,
) -> Result<PathBuf> {
    let repo_root = prepared.git(&GitSource::new(repo, git_ref))?;
    match path {
        Some(path) => resolve_repo_path(&repo_root, path),
        None => Ok(repo_root),
    }
}

/// Fetch a git repository into the local cache.
pub(crate) fn fetch_git_repo(source: &GitSource, base_dir: &Path) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let repo_url = &source.repo;
    let git_ref = source.git_ref.as_deref();
    let key = format!("{:x}", Sha256::digest(serde_json::to_vec(source)?));
    let repo_dir = base_dir.join("repos").join(key);

    if repo_dir.exists() {
        if let Some(commit) = git_ref
            .filter(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            let object = Command::new("git")
                .arg("-C")
                .arg(&repo_dir)
                .args(["cat-file", "-t", commit])
                .output()?;
            if object.status.success() && object.stdout == b"commit\n" {
                let inspect = |args: &[&str]| {
                    Command::new("git")
                        .arg("-C")
                        .arg(&repo_dir)
                        .env("GIT_OPTIONAL_LOCKS", "0")
                        .args(args)
                        .output()
                };
                let head = inspect(&["rev-parse", "HEAD"])?;
                let status = inspect(&["status", "--porcelain", "--untracked-files=no"])?;
                if !head.status.success()
                    || !String::from_utf8_lossy(&head.stdout)
                        .trim()
                        .eq_ignore_ascii_case(commit)
                    || !status.status.success()
                    || !status.stdout.is_empty()
                {
                    run_git(&repo_dir, &["reset", "--hard", "--quiet", commit])?;
                }
                return Ok(repo_dir);
            }
        }
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
                .arg(repo_url)
                .arg(&repo_dir),
        )
        .with_context(|| format!("failed to clone git skill repository {repo_url}"))?;
    }

    if let Some(requested_ref) = git_ref {
        run_git(&repo_dir, &["checkout", "--quiet", requested_ref])?;
        if checked_out_branch(&repo_dir)?.is_some() {
            run_git(&repo_dir, &["merge", "--ff-only", "--quiet", "@{upstream}"])?;
        }
    } else {
        run_git(&repo_dir, &["merge", "--ff-only", "--quiet", "@{upstream}"])?;
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

/// A skill source must be a directory containing a `SKILL.md`.
fn validate_skill_dir(skill_name: &str, source: &Path) -> Result<()> {
    if !crate::skill::is_skill_dir(source) {
        if !source.is_dir() {
            return Err(anyhow::anyhow!(
                "skill '{}' source directory not found: {}",
                skill_name,
                source.display()
            ));
        }
        return Err(anyhow::anyhow!(
            "skill '{}' source has no SKILL.md: {}",
            skill_name,
            source.display()
        ));
    }
    Ok(())
}

fn copy_skill_dir(skill_name: &str, source: &Path, dest: &Path) -> Result<()> {
    validate_skill_dir(skill_name, source)?;
    copy_dir_all(source, dest)
}

/// Live-link a skill: symlink `dest` at the source working tree so edits flow
/// both ways. The symlink points at the source's canonical absolute path, so it
/// survives even if the source was declared relative to the config.
fn link_skill_dir(skill_name: &str, source: &Path, dest: &Path) -> Result<()> {
    validate_skill_dir(skill_name, source)?;
    let target = fs::canonicalize(source).with_context(|| {
        format!(
            "failed to resolve skill '{}' source path {}",
            skill_name,
            source.display()
        )
    })?;
    crate::linker::create_symlink(&target, dest).with_context(|| {
        format!(
            "failed to link skill '{}' at {}",
            skill_name,
            dest.display()
        )
    })?;
    Ok(())
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

fn checked_out_branch(repo_dir: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .with_context(|| {
            format!(
                "failed to inspect checked-out branch in {}",
                repo_dir.display()
            )
        })?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(Some(branch))
    } else {
        Ok(None)
    }
}

/// Download a single skill URL to the destination path.
/// Skips if the file exists and has the same size as the remote content.
fn download_skill(url: &str, dest: &Path) -> Result<()> {
    // Check existing file first — fast path for unchanged skills.
    let existing_size = fs::metadata(dest).ok().map(|m| m.len());

    let response = ureq::get(url)
        .timeout(std::time::Duration::from_secs(30))
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

pub(crate) fn cache_url(url: &str, cache: &Path) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};

    let dir = cache
        .join("downloads")
        .join(format!("{:x}", Sha256::digest(url.as_bytes())));
    fs::create_dir_all(&dir)?;
    let path = dir.join("SKILL.md");
    download_skill(url, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximum_length_bundle_keys_install_update_and_recover() {
        for length in [245, 246, 255] {
            let temp = tempfile::tempdir().unwrap();
            let name = "a".repeat(length);
            let source = temp.path().join("source");
            write_skill_dir(&source.join("skills/demo"), "first");
            let mut bundle = bundle_with(
                SourceSpec {
                    source: Some(source.to_string_lossy().into_owned()),
                    ..SourceSpec::default()
                },
                &[],
            );
            bundle.link = false;
            let cache = temp.path().join("cache");
            let root = assemble_bundle(&name, &bundle, &cache, temp.path()).unwrap();
            fs::write(source.join("skills/demo/SKILL.md"), "updated").unwrap();
            assemble_bundle(&name, &bundle, &cache, temp.path()).unwrap();
            let backup = crate::cache::Layout::resolve(&cache)
                .unwrap()
                .bundle(&name)
                .unwrap()
                .previous;
            assert!(backup.file_name().unwrap().len() <= 255);
            fs::rename(&root, &backup).unwrap();
            fs::remove_dir_all(&source).unwrap();
            assert!(assemble_bundle(&name, &bundle, &cache, temp.path()).is_err());
            assert_eq!(
                fs::read_to_string(root.join("skills/demo/SKILL.md")).unwrap(),
                "updated"
            );
            assert!(!backup.exists());
        }
    }

    fn normalized_repo(repo: &str) -> String {
        GitSource::new(repo, None).repo
    }

    fn fetch_git_repo(repo: &str, git_ref: Option<&str>, cache: &Path) -> Result<PathBuf> {
        super::fetch_git_repo(&GitSource::new(repo, git_ref), cache)
    }

    /// Build a bundle from an optional whole-bundle source plus explicit skills.
    /// `link` is true (the default) unless a test overrides `.link`.
    fn bundle_with(source: SourceSpec, skills: &[(&str, SourceSpec)]) -> Bundle {
        Bundle {
            harnesses: Vec::new(),
            source,
            skills: skills
                .iter()
                .map(|(name, spec)| ((*name).to_string(), spec.clone()))
                .collect(),
            link: true,
        }
    }

    /// Write a minimal skill directory (a `SKILL.md`) at `dir`.
    fn write_skill_dir(dir: &Path, name: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\n---\n")).unwrap();
    }

    /// Initialize `dir` as a git repo with a single commit of its contents.
    fn init_git_repo(dir: &Path) {
        run_command(Command::new("git").arg("init").arg(dir)).unwrap();
        run_git(dir, &["config", "user.email", "test@example.com"]).unwrap();
        run_git(dir, &["config", "user.name", "Test User"]).unwrap();
        run_git(dir, &["add", "."]).unwrap();
        run_git(dir, &["commit", "--quiet", "-m", "init"]).unwrap();
    }

    fn local_skill_bundle(source: &Path) -> Bundle {
        bundle_with(
            SourceSpec::default(),
            &[(
                "code-design",
                SourceSpec {
                    source: Some(source.to_string_lossy().to_string()),
                    ..SourceSpec::default()
                },
            )],
        )
    }

    #[test]
    fn test_assemble_bundle_copies_local_skill_when_link_false() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("cache");
        let source = tmp.path().join("source").join("code-design");
        fs::create_dir_all(source.join("agents")).unwrap();
        fs::write(source.join("SKILL.md"), "---\nname: code-design\n---\n").unwrap();
        fs::write(source.join("agents").join("openai.yaml"), "version: 1\n").unwrap();

        let mut bundle = local_skill_bundle(&source);
        bundle.link = false;

        let bundle_root = assemble_bundle("generic", &bundle, &base, tmp.path()).unwrap();

        let installed = bundle_root.join("skills").join("code-design");
        // A copy: a real directory, not a symlink, with the companion file.
        assert!(!installed
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(installed.join("SKILL.md").exists());
        assert!(installed.join("agents").join("openai.yaml").exists());
    }

    #[test]
    fn test_assemble_bundle_links_local_skill_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("cache");
        let source = tmp.path().join("source").join("code-design");
        write_skill_dir(&source, "code-design");

        // Default bundle.link == true.
        let bundle = local_skill_bundle(&source);
        let bundle_root = assemble_bundle("generic", &bundle, &base, tmp.path()).unwrap();

        let installed = bundle_root.join("skills").join("code-design");
        // A live link: the assembled entry is a symlink at the source tree.
        assert!(installed
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_link(&installed).unwrap(),
            fs::canonicalize(&source).unwrap()
        );

        // Edits are bidirectional: writing through the link reaches the source.
        fs::write(installed.join("SKILL.md"), "EDITED VIA HARNESS").unwrap();
        assert_eq!(
            fs::read_to_string(source.join("SKILL.md")).unwrap(),
            "EDITED VIA HARNESS"
        );
    }

    #[test]
    fn test_assemble_bundle_failure_keeps_existing_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let existing_skill = cache
            .join("bundles")
            .join("generic")
            .join("skills")
            .join("code-design");
        write_skill_dir(&existing_skill, "old-code-design");

        let invalid_source = tmp.path().join("invalid-bundle");
        fs::create_dir_all(&invalid_source).unwrap();
        let bundle = bundle_with(
            SourceSpec {
                source: Some(invalid_source.to_string_lossy().to_string()),
                ..SourceSpec::default()
            },
            &[],
        );

        let result = assemble_bundle("generic", &bundle, &cache, tmp.path());

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(existing_skill.join("SKILL.md")).unwrap(),
            "---\nname: old-code-design\n---\n"
        );
    }

    #[test]
    fn test_assemble_bundle_downloads_url_skill() {
        let Some(url) = serve_once("# Remote Skill\n\nTest content") else {
            return;
        };

        let tmp = tempfile::tempdir().unwrap();
        let bundle = bundle_with(
            SourceSpec::default(),
            &[(
                "remote-skill",
                SourceSpec {
                    url: Some(url),
                    ..SourceSpec::default()
                },
            )],
        );

        let bundle_root =
            assemble_bundle("remote", &bundle, &tmp.path().join("cache"), tmp.path()).unwrap();

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
    fn test_assemble_bundle_pulls_whole_local_bundle_source() {
        // Point a bundle at a local bundle directory; every skill under its
        // skills/ folder is pulled as a unit, with no explicit entries.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp
            .path()
            .join("agent-skills")
            .join("bundles")
            .join("generic");
        write_skill_dir(&src.join("skills").join("code-design"), "code-design");
        write_skill_dir(
            &src.join("skills").join("technical-writing"),
            "technical-writing",
        );
        // A stray file under skills/ must be ignored (not a skill directory).
        fs::write(src.join("skills").join("README.md"), "ignore me").unwrap();

        let bundle = bundle_with(
            SourceSpec {
                source: Some(src.to_string_lossy().to_string()),
                ..SourceSpec::default()
            },
            &[],
        );

        let bundle_root =
            assemble_bundle("generic", &bundle, &tmp.path().join("cache"), tmp.path()).unwrap();

        let skills = bundle_root.join("skills");
        assert!(skills.join("code-design").join("SKILL.md").exists());
        assert!(skills.join("technical-writing").join("SKILL.md").exists());
        assert!(!skills.join("README.md").exists());
        // Local whole-bundle source links live by default.
        assert!(skills
            .join("code-design")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn test_assemble_bundle_pulls_whole_git_bundle_source() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("agent-skills");
        write_skill_dir(
            &repo
                .join("bundles")
                .join("generic")
                .join("skills")
                .join("code-design"),
            "code-design",
        );
        write_skill_dir(
            &repo
                .join("bundles")
                .join("generic")
                .join("skills")
                .join("context7-mcp"),
            "context7-mcp",
        );
        init_git_repo(&repo);

        let bundle = bundle_with(
            SourceSpec {
                repo: Some(repo.to_string_lossy().to_string()),
                path: Some("bundles/generic".to_string()),
                ..SourceSpec::default()
            },
            &[],
        );

        let bundle_root =
            assemble_bundle("generic", &bundle, &tmp.path().join("cache"), tmp.path()).unwrap();

        // A git whole-bundle source is copied even with link on (default): the
        // git cache is not a working tree to link against.
        assert!(!bundle_root
            .join("skills")
            .join("code-design")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(bundle_root
            .join("skills")
            .join("code-design")
            .join("SKILL.md")
            .exists());
        assert!(bundle_root
            .join("skills")
            .join("context7-mcp")
            .join("SKILL.md")
            .exists());
    }

    #[test]
    fn test_assemble_bundle_source_plus_explicit_skill_overrides() {
        // Layer explicit skills over a whole-bundle source: an extra skill is
        // added, and a same-named skill overrides the one from the source.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("bundle");
        write_skill_dir(&src.join("skills").join("shared"), "from-source");

        let override_dir = tmp.path().join("override").join("shared");
        fs::create_dir_all(&override_dir).unwrap();
        fs::write(override_dir.join("SKILL.md"), "OVERRIDDEN").unwrap();

        let extra_dir = tmp.path().join("extra").join("caveman");
        write_skill_dir(&extra_dir, "caveman");

        let bundle = bundle_with(
            SourceSpec {
                source: Some(src.to_string_lossy().to_string()),
                ..SourceSpec::default()
            },
            &[
                (
                    "shared",
                    SourceSpec {
                        source: Some(override_dir.to_string_lossy().to_string()),
                        ..SourceSpec::default()
                    },
                ),
                (
                    "caveman",
                    SourceSpec {
                        source: Some(extra_dir.to_string_lossy().to_string()),
                        ..SourceSpec::default()
                    },
                ),
            ],
        );

        let bundle_root =
            assemble_bundle("generic", &bundle, &tmp.path().join("cache"), tmp.path()).unwrap();

        let skills = bundle_root.join("skills");
        assert!(skills.join("caveman").join("SKILL.md").exists());
        let shared = fs::read_to_string(skills.join("shared").join("SKILL.md")).unwrap();
        assert_eq!(shared, "OVERRIDDEN");
    }

    #[test]
    fn test_assemble_bundle_rejects_url_bundle_source() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = bundle_with(
            SourceSpec {
                url: Some("https://example.com/SKILL.md".to_string()),
                ..SourceSpec::default()
            },
            &[],
        );

        let result = assemble_bundle("generic", &bundle, &tmp.path().join("cache"), tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_assemble_bundle_git_skill_defaults_to_repo_root() {
        // A git skill without `path` resolves to the repo root as the skill dir.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("single-skill-repo");
        write_skill_dir(&repo, "solo");
        init_git_repo(&repo);

        let bundle = bundle_with(
            SourceSpec::default(),
            &[(
                "solo",
                SourceSpec {
                    repo: Some(repo.to_string_lossy().to_string()),
                    ..SourceSpec::default()
                },
            )],
        );

        let bundle_root =
            assemble_bundle("generic", &bundle, &tmp.path().join("cache"), tmp.path()).unwrap();
        assert!(bundle_root
            .join("skills")
            .join("solo")
            .join("SKILL.md")
            .exists());
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
            normalized_repo("lutyjj/agent-skills"),
            "git@github.com:lutyjj/agent-skills.git"
        );
        assert_eq!(
            normalized_repo("gh:lutyjj/agent-skills"),
            "git@github.com:lutyjj/agent-skills.git"
        );
        assert_eq!(
            normalized_repo("github:lutyjj/agent-skills.git"),
            "git@github.com:lutyjj/agent-skills.git"
        );
        assert_eq!(
            normalized_repo("https://github.com/lutyjj/agent-skills.git"),
            "https://github.com/lutyjj/agent-skills.git"
        );
    }

    #[test]
    fn cached_immutable_commit_remains_usable_without_its_origin() {
        use std::collections::BTreeMap;
        use std::os::unix::fs::MetadataExt;

        fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, i64, i64, Vec<u8>)> {
            let mut result = BTreeMap::new();
            let meta = fs::symlink_metadata(root).unwrap();
            let bytes = if meta.is_file() {
                fs::read(root).unwrap()
            } else {
                Vec::new()
            };
            result.insert(
                root.to_path_buf(),
                (meta.ino(), meta.mtime(), meta.mtime_nsec(), bytes),
            );
            if meta.is_dir() {
                for entry in fs::read_dir(root).unwrap() {
                    result.extend(snapshot(&entry.unwrap().path()));
                }
            }
            result
        }

        let temp = tempfile::tempdir().unwrap();
        let origin = temp.path().join("origin");
        write_skill_dir(&origin, "fixture");
        init_git_repo(&origin);
        let output = Command::new("git")
            .arg("-C")
            .arg(&origin)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let commit = String::from_utf8(output.stdout).unwrap().trim().to_string();
        let cache = temp.path().join("cache");
        let first = fetch_git_repo(origin.to_str().unwrap(), Some(&commit), &cache).unwrap();
        fs::remove_dir_all(&origin).unwrap();
        let before = snapshot(&first);
        let second = fetch_git_repo(origin.to_str().unwrap(), Some(&commit), &cache).unwrap();
        assert_eq!(first, second);
        assert_eq!(snapshot(&second), before);
        assert!(second.join("SKILL.md").exists());
        let original = fs::read(second.join("SKILL.md")).unwrap();
        fs::write(second.join("SKILL.md"), b"dirty").unwrap();
        fetch_git_repo(origin.to_str().unwrap(), Some(&commit), &cache).unwrap();
        assert_eq!(fs::read(second.join("SKILL.md")).unwrap(), original);
        fs::write(second.join("SKILL.md"), b"another commit").unwrap();
        run_git(&second, &["add", "."]).unwrap();
        run_git(
            &second,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
                "commit",
                "--quiet",
                "-m",
                "different",
            ],
        )
        .unwrap();
        fetch_git_repo(origin.to_str().unwrap(), Some(&commit), &cache).unwrap();
        assert_eq!(fs::read(second.join("SKILL.md")).unwrap(), original);
        let converged = snapshot(&second);
        fetch_git_repo(origin.to_str().unwrap(), Some(&commit), &cache).unwrap();
        assert_eq!(snapshot(&second), converged);
        assert!(fetch_git_repo(origin.to_str().unwrap(), Some("main"), &cache).is_err());
    }

    #[test]
    fn url_cache_names_are_opaque_and_bounded_for_signed_urls() {
        let temp = tempfile::tempdir().unwrap();
        for credential in [
            "credential-marker".to_string(),
            "credential-marker".repeat(30),
        ] {
            let Some(base) = serve_once("skill") else {
                return;
            };
            let path = cache_url(&format!("{base}?token={credential}"), temp.path()).unwrap();
            assert_eq!(fs::read(path).unwrap(), b"skill");
        }
        let names: Vec<_> = fs::read_dir(temp.path().join("downloads"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names
            .iter()
            .all(|name| name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())));
    }

    #[test]
    fn test_fetch_git_repo_reports_failed_pull() {
        let tmp = tempfile::tempdir().unwrap();
        let source_repo = tmp.path().join("source");
        write_skill_dir(&source_repo, "source");
        init_git_repo(&source_repo);

        let cache = tmp.path().join("cache");
        let cached_repo = fetch_git_repo(&source_repo.to_string_lossy(), None, &cache).unwrap();
        fs::write(cached_repo.join("SKILL.md"), "dirty local cache").unwrap();

        fs::write(source_repo.join("SKILL.md"), "remote update").unwrap();
        run_git(&source_repo, &["add", "."]).unwrap();
        run_git(&source_repo, &["commit", "--quiet", "-m", "update"]).unwrap();

        let result = fetch_git_repo(&source_repo.to_string_lossy(), None, &cache);

        assert!(result.is_err());
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
    fn test_assemble_bundle_uses_git_skill_path() {
        let tmp = tempfile::tempdir().unwrap();
        let source_repo = tmp.path().join("source");
        write_skill_dir(
            &source_repo
                .join("bundles")
                .join("generic")
                .join("skills")
                .join("code-design"),
            "code-design",
        );
        init_git_repo(&source_repo);

        let bundle = bundle_with(
            SourceSpec::default(),
            &[(
                "code-design",
                SourceSpec {
                    repo: Some(source_repo.to_string_lossy().to_string()),
                    path: Some("bundles/generic/skills/code-design".to_string()),
                    ..SourceSpec::default()
                },
            )],
        );

        let bundle_root =
            assemble_bundle("generic", &bundle, &tmp.path().join("cache"), tmp.path()).unwrap();

        assert!(bundle_root
            .join("skills")
            .join("code-design")
            .join("SKILL.md")
            .exists());
    }

    #[test]
    fn test_assemble_bundle_rejects_invalid_skill_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = bundle_with(
            SourceSpec::default(),
            &[(
                "bad",
                SourceSpec {
                    url: Some("https://example.com/skill.md".to_string()),
                    source: Some("/tmp/skill".to_string()),
                    ..SourceSpec::default()
                },
            )],
        );

        let result = assemble_bundle("generic", &bundle, &tmp.path().join("cache"), tmp.path());
        assert!(result.is_err());
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
