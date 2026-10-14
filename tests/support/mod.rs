/// Use physical roots so implicit temporary-directory aliases do not change expected identities.
pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)
}
