mod support;

use std::fs;
use std::process::Command;

#[test]
fn publication_requires_a_matching_annotated_tag_on_the_built_commit() {
    for case in [
        "branch",
        "lightweight",
        "wrong-version",
        "different-commit",
        "valid",
    ] {
        let temp = support::tempdir().unwrap();
        let command = |program: &str| {
            let mut command = Command::new(program);
            command
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap())
                .env("HOME", temp.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(temp.path());
            command
        };
        let git = |args: &[&str]| {
            let output = command("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "Fixture"]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nversion = \"0.3.0\"\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "fixture"]);
        let tag = if case == "wrong-version" {
            "v0.2.0"
        } else {
            "v0.3.0"
        };
        if case == "lightweight" {
            git(&["tag", tag]);
        } else {
            git(&["tag", "-a", tag, "-m", "fixture"]);
        }
        if case == "different-commit" {
            git(&["commit", "--allow-empty", "-qm", "another commit"]);
        }
        let reference = if case == "branch" {
            "refs/heads/main".into()
        } else {
            format!("refs/tags/{tag}")
        };
        let output_path = temp.path().join("output");
        let output = command("sh")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/release-version.sh"
            ))
            .env("GITHUB_REF", reference)
            .env("GITHUB_OUTPUT", &output_path)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            case == "valid",
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if case == "valid" {
            assert_eq!(
                fs::read_to_string(&output_path).unwrap(),
                format!(
                    "version=0.3.0\ntag=v0.3.0\ncommit={}\n",
                    git(&["rev-parse", "HEAD"])
                )
            );
        } else {
            assert!(!output_path.exists());
        }
    }
}
