use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn publish_executable(path: &Path) {
    let staged = path.with_extension("staged");
    fs::copy(path, &staged).unwrap();
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(staged, path).unwrap();
}
