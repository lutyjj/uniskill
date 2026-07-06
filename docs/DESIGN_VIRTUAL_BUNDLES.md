# Design: virtual bundles

Wire skills from remote URLs into agent harnesses alongside local bundles. A single bundle can hold any number of skills, each defined by a URL that resolves to a `.md` file.

## Contract

A bundle is declared under `[bundles.<name>]` in config. It has either a `source` path (local directory) or a `skills` map (remote URLs). Each bundle is wired into the harnesses it declares.

The user experience:
1. Declare bundles with local paths or remote skill URLs.
2. Run `uniskill sync`.
3. Skills appear at harness locations as symlinks — local ones point to their source directory, virtual ones point to a local cache.
4. Re-sync re-downloads changed URLs and updates symlinks.

What virtual bundles do not do:
- They do not skip the symlink layer; installed skills go through harness pattern resolution exactly like local bundles.
- They do not stream directly into agent directories; caching is required because the linker works on filesystem paths.

## Config schema

Bundles are a **map** keyed by bundle name under `[bundles]`. Each entry declares its source and target harnesses:

### Local bundle (existing)

```toml
[bundles.my-skills]
source = "$HOME/.dotfiles/skills"
harnesses = ["pi", "claude-code"]
```

`source` is the path to a directory containing a `skills/` subfolder. This is the existing format, unchanged.

### Virtual bundle

Add a `skills` map under the same bundle entry. When `skills` is present alongside (or instead of) `source`, the bundle becomes virtual:

```toml
[bundles.important-stuff]
harnesses = ["pi"]

[bundles.important-stuff.skills.caveman]
url = "https://raw.githubusercontent.com/JuliusBrussee/caveman/refs/heads/main/skills/caveman/SKILL.md"

[bundles.important-stuff.skills.code-design]
url = "https://raw.githubusercontent.com/example/code-design/refs/heads/main/SKILL.md"
```

The `skills.` map key (`caveman`, `code-design`) is the skill name — it determines directory name, symlink name, and everything downstream. Each entry has at least a `url` field. Future fields like `path_suffix` or `checksum` can be added without changing the outer structure.

### Mixed bundle (future)

A bundle with both `source` and `skills` downloads remote skills into the same cache while keeping local skills on disk. This lets users group curated URLs with local work-in-progress under one harness target.

## Data model changes

### config.rs additions

Bundles become a map instead of an array:

```rust
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Bundle definitions keyed by bundle name.
    /// Local bundles have `source`; virtual bundles have `skills`.
    #[serde(default)]
    pub bundles: HashMap<String, Bundle>,
}

#[derive(Debug, Deserialize)]
pub struct Bundle {
    /// Path to local bundle root; ignored when skills is present.
    pub source: Option<String>,

    /// Which harnesses to wire this bundle into.
    pub harnesses: Vec<String>,

    /// Remote skill definitions for virtual bundles.
    #[serde(default)]
    pub skills: HashMap<String, SkillEntry>,
}

#[derive(Debug, Deserialize)]
pub struct SkillEntry {
    /// HTTP(S) URL to fetch the skill markdown file.
    pub url: String,

    /// Optional sub-path within a directory URL (future).
    pub path_suffix: Option<String>,

    /// Optional checksum for content verification (future).
    pub checksum: Option<String>,
}
```

TOML deserialisation flow:
- `[bundles.my-skills]` → `HashMap::insert("my-skills", Bundle { source: Some(...), skills: {} })`
- `[bundles.important-stuff.skills.caveman]` → `HashMap::insert("caveman", SkillEntry { url: "..." })`

This keeps naming consistent with existing code (S5): the map key is the skill name everywhere it matters — config, directory creation, symlink naming.

### fetcher.rs — new module

A new file `src/fetcher.rs` handles downloading and caching:

```rust
/// Download skills from URLs into a local cache directory.
/// Returns path to the assembled bundle directory.
pub fn assemble_virtual_bundle(
    bundle_name: &str,
    skills: &HashMap<String, SkillEntry>,
    base_dir: &Path,
) -> Result<PathBuf>
```

The function:
1. Creates `{base_dir}/{bundle_name}/skills/{skill_name}/` for each entry in the map.
2. Downloads `url` to `SKILL.md` in that directory (skips if file exists and content hash matches).
3. Returns the assembled path, which the linker then treats as a normal bundle source.

The cache lives at `$XDG_CACHE_HOME/uniskill/` (or `~/.cache/uniskill/` on macOS), giving each virtual bundle its own subdirectory. Cached skills are machine-local and isolated from the working tree. If the user removes `~/.cache/uniskill/`, cached skills become broken symlinks — the next sync re-downloads them.

### cli.rs changes

Changes to `sync_with_registry`. The decision between local and virtual happens per-bundle at iteration time:

```rust
for (bundle_name, bundle) in &config.bundles {
    // Resolve harnesses...

    let source = match (&bundle.source, &bundle.skills) {
        (Some(path), _) => config::resolve_source(path),
        (None, skills) if !skills.is_empty() => assembler::assemble_virtual_bundle(
            bundle_name,
            skills,
            &cache_dir,
        )?,
        _ => return Err(...),  // neither source nor skills defined
    };

    // Rest of the sync logic unchanged — source is always a PathBuf
    let results = linker::sync_bundle(&source, &harness.pattern);
}
```

### Cargo.toml dependency addition

Add `ureq` for synchronous HTTP fetching:

```toml
[dependencies]
# ... existing deps ...
ureq = { version = "2", features = ["tls"] }
```

`ureq` is preferred over `reqwest` because bundle assembly is inherently synchronous — we must download all skills before any symlinks can be created — and it has a smaller dependency footprint.

## Sync behavior

| Situation | Status | Action |
|-----------|--------|--------|
| First sync, URL valid | Created | Download → cache → symlink |
| Re-sync, file unchanged (content hash matches) | Ok | Skip download, keep symlink |
| Re-sync, file changed (new content hash) | Updated | Re-download → replace cached file |
| URL unreachable | Broken | Skip this skill, continue with others in bundle |
| Target slot has non-symlink file | Conflict | Skip this skill (same as local bundles) |

Individual failed downloads do not abort the entire bundle. Each skill is independent — one broken URL does not prevent other skills from installing. This makes the tool resilient to flaky upstream URLs.

## S15 compliance: adding new source types

Adding a third bundle type (e.g., git repo bundles) would add `git: Option<GitEntry>` to `Bundle` and extend the match expression with one arm. No existing local or virtual logic is edited. ✓

## S8 compliance: separated responsibilities

| Module | Responsibility |
|--------|---------------|
| config.rs | Parse bundle types (local vs virtual) |
| fetcher.rs | Download URLs, manage cache lifecycle |
| linker.rs | Symlink creation — unchanged, receives a path |
| cli.rs | Orchestrate local → resolve_path / virtual → assemble_cache |

The linker does not need to know about URLs. It still receives `PathBuf` and creates symlinks. ✓

## S12 compliance: one source of truth

Skill name is defined once — as the TOML map key (`[bundles.important-stuff.skills.caveman]`). This flows through config parsing → hashmap lookup → directory creation → symlink naming. No duplicate naming anywhere. ✓

## Testing plan (S4)

| Test | Input | Expected output |
|------|-------|-----------------|
| `test_assemble_creates_correct_layout` | Two skills with URLs, temp base dir | Files at `{base}/bundle/skills/{name}/SKILL.md`, path returned matches |
| `test_download_skips_unchanged` | Skill file exists, URL returns same content | Second call skips download (verified by checking fetch count) |
| `test_download_replaces_changed` | Skill file exists with different content, URL returns new | File overwritten with new content |
| `test_failed_skill_does_not_abort_bundle` | One broken URL, one valid URL in same bundle | Broken skill reported, valid skill installs successfully |
| `test_parse_local_bundle_from_toml` | TOML with `[bundles.x] source="..." harnesses=["pi"]` | Bundle has `source`, empty skills map |
| `test_parse_virtual_bundle_from_toml` | TOML with nested `skills.` section | Bundle has `source: None`, skills map populated correctly |
| `test_parse_mixed_bundle_from_toml` | TOML with both `source` and `skills.` | Bundle has both fields populated |

Tests for the fetcher use an in-process HTTP server (`tiny_http`) so they do not depend on network availability or external URLs.

## Files touched

| File | Change |
|------|--------|
| `Cargo.toml` | Add `ureq` dependency |
| `src/config.rs` | Change `bundles: Vec<Bundle>` → `HashMap<String, Bundle>`, add `SkillEntry` struct, make `source: Option<String>` |
| `src/fetcher.rs` | New file: download + cache assembly |
| `src/cli.rs` | Replace bundle iteration (`for bundle in bundles`) with map iteration (`for (name, bundle) in bundles`), add virtual branch |
| `tests/...` | Add tests for TOML deserialization and fetcher logic |

## Risks and tradeoffs

1. **Network dependency**: Virtual bundles require outbound HTTPS. Offline environments cannot resolve them without pre-caching. The tool reports which skills failed to download so the user can handle offline files manually.

2. **Cache eviction**: Downloaded files persist until removed explicitly or cache is cleared. No TTL or size limit in v1. This is acceptable because virtual bundles are typically small and change infrequently. Future work could add a `--cache-cleanup` command or automatic TTL based on response headers.

3. **Content verification**: We do not validate downloaded content (no checksums, no HTTPS cert pinning). For a v1 CLI tool this is an accepted risk — the user supplies the URLs and trusts them, same as mounting a local path they trust. Adding signature verification would be a separate feature.

4. **TOML breaking change**: Changing `bundles` from `Vec<Bundle>` to `HashMap<String, Bundle>` breaks existing config files that use `[[bundles]]`. Migration: replace `[[bundles]]` entries with `[bundles.<name>]` blocks. The migration path is straightforward because the tool can show a helpful error message suggesting the new format.
