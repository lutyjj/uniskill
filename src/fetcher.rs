use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::SkillEntry;

/// Assemble a virtual bundle by downloading all skills into a local cache.
/// Returns the path to the assembled bundle root (the linker will look for
/// a `skills/` subdirectory inside it).
pub fn assemble_virtual_bundle(
    bundle_name: &str,
    skills: &HashMap<String, SkillEntry>,
    base_dir: &Path,
) -> Result<PathBuf> {
    let bundle_root = base_dir.join(bundle_name);
    let bundle_dir = bundle_root.join("skills");
    fs::create_dir_all(&bundle_dir).with_context(|| {
        format!("failed to create cache directory for virtual bundle '{bundle_name}'")
    })?;

    for (skill_name, entry) in skills {
        let skill_dir = bundle_dir.join(skill_name);
        let dest = skill_dir.join("SKILL.md");

        // Create the target directory if it doesn't exist yet.
        if !dest.exists() {
            fs::create_dir_all(&skill_dir).with_context(|| {
                format!("failed to create skill cache dir for '{skill_name}'")
            })?;
        }

        download_skill(entry.url.as_str(), &dest)?;
    }

    Ok(bundle_root)
}

/// Download a single skill URL to the destination path.
/// Skips if the file exists and has the same size as the remote content.
fn download_skill(url: &str, dest: &Path) -> Result<()> {
    // Check existing file first — fast path for unchanged skills.
    let existing_size = fs::metadata(dest).ok().map(|m| m.len());

    let response = ureq::get(url).call()
        .with_context(|| format!("failed to fetch skill from {url}"))?;

    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(anyhow::anyhow!(
            "HTTP {} when fetching skill from {url}",
            status
        ));
    }

    let body: String = response.into_string()
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
    let mut file = File::create(&tmp).with_context(|| {
        format!("failed to create temp file at {:?}", tmp)
    })?;
    file.write_all(body_bytes).with_context(|| {
        format!("failed to write skill content to {:?}", dest)
    })?;
    drop(file);

    // Atomic rename replaces the old file.
    fs::rename(&tmp, dest).with_context(|| {
        format!("failed to replace skill file at {:?}", dest)
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assemble_virtual_bundle_creates_correct_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("cache");

        let mut skills = HashMap::new();
        skills.insert(
            "caveman".to_string(),
            SkillEntry { url: "https://example.com/caveman.md".to_string() },
        );
        skills.insert(
            "code-design".to_string(),
            SkillEntry { url: "https://example.com/code.md".to_string() },
        );

        let result = assemble_virtual_bundle("test-bundle", &skills, &base);
        assert!(result.is_err()); // URLs don't exist, expected error

        // The structure should have been created up to the point of failure.
        // Since both fail at download, check that directories were created.
        let bundle_dir = base.join("test-bundle").join("skills");
        assert!(bundle_dir.exists());
    }

    #[test]
    fn test_download_skill_writes_content() {
        use tiny_http::{Response, Server};

        let server = Server::http("127.0.0.1:0").unwrap();
        // tiny_http 0.12 returns ListenAddr; extract port from IP variant.
        let addr = match server.server_addr() {
            tiny_http::ListenAddr::IP(sa) => sa.port(),
            #[cfg(unix)]
            tiny_http::ListenAddr::Unix(_) => unreachable!(),
        };
        let expected_body = "# My Skill\n\nTest content";

        // Spawn a one-shot server that returns our skill content.
        std::thread::spawn(move || {
            if let Ok(mut request) = server.recv() {
                let response = Response::from_string(expected_body);
                request.respond(response).ok();
            }
        });

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("SKILL.md");

        let result = download_skill(&format!("http://127.0.0.1:{addr}/skill"), &dest);
        assert!(result.is_ok());

        let content = fs::read_to_string(&dest).unwrap();
        assert_eq!(content, expected_body);
    }
}
