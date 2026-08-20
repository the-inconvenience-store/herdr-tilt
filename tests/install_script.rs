#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const TARGETS: [(&str, &str, &str); 4] = [
    ("Darwin", "arm64", "herdr-tilt-macos-aarch64"),
    ("Darwin", "x86_64", "herdr-tilt-macos-x86_64"),
    ("Linux", "aarch64", "herdr-tilt-linux-aarch64"),
    ("Linux", "x86_64", "herdr-tilt-linux-x86_64"),
];

#[test]
fn installer_downloads_and_verifies_each_supported_release_asset() {
    for (os, arch, asset) in TARGETS {
        let fixture = InstallFixture::new(asset, false);

        let output = fixture.install(os, arch);
        assert!(
            output.status.success(),
            "installer failed for {os}/{arch}: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let installed = fixture.root.join("target/release/herdr-tilt");
        assert_eq!(fs::read(&installed).unwrap(), fixture.binary);
        assert_ne!(
            fs::metadata(installed).unwrap().permissions().mode() & 0o111,
            0,
            "installed binary must be executable"
        );
    }
}

#[test]
fn installer_rejects_a_bad_checksum_without_replacing_the_binary() {
    let fixture = InstallFixture::new("herdr-tilt-linux-x86_64", true);
    let installed = fixture.root.join("target/release/herdr-tilt");
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    fs::write(&installed, b"existing binary").unwrap();

    let output = fixture.install("Linux", "x86_64");

    assert!(!output.status.success());
    assert_eq!(fs::read(installed).unwrap(), b"existing binary");
}

struct InstallFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    release: PathBuf,
    fake_bin: PathBuf,
    binary: Vec<u8>,
}

impl InstallFixture {
    fn new(asset: &str, bad_checksum: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("plugin");
        let release = temp.path().join("release");
        let fake_bin = temp.path().join("fake-bin");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::create_dir_all(&release).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        fs::copy("scripts/install.sh", root.join("scripts/install.sh")).unwrap();
        fs::write(
            root.join("herdr-plugin.toml"),
            "id = \"herdr.tilt\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let binary = format!("prebuilt fixture for {asset}\n").into_bytes();
        fs::write(release.join(asset), &binary).unwrap();
        let checksum = if bad_checksum {
            "0".repeat(64)
        } else {
            format!("{:x}", Sha256::digest(&binary))
        };
        fs::write(
            release.join(format!("{asset}.sha256")),
            format!("{checksum}  {asset}\n"),
        )
        .unwrap();

        write_executable(
            &fake_bin.join("curl"),
            "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    --output) output=$2; shift 2 ;;\n    http*) url=$1; shift ;;\n    *) shift ;;\n  esac\ndone\ncp \"$FAKE_RELEASE_DIR/${url##*/}\" \"$output\"\n",
        );

        Self {
            _temp: temp,
            root,
            release,
            fake_bin,
            binary,
        }
    }

    fn install(&self, os: &str, arch: &str) -> std::process::Output {
        let path = format!(
            "{}:{}",
            self.fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new("/bin/sh")
            .arg(self.root.join("scripts/install.sh"))
            .current_dir(&self.root)
            .env("PATH", path)
            .env("FAKE_RELEASE_DIR", &self.release)
            .env("HERDR_TILT_INSTALL_OS", os)
            .env("HERDR_TILT_INSTALL_ARCH", arch)
            .env("HERDR_TILT_RELEASE_BASE_URL", "https://release.test/v0.1.0")
            .output()
            .unwrap()
    }
}

fn write_executable(path: &Path, contents: &str) {
    let staged = path.with_extension("staged");
    fs::write(&staged, contents).unwrap();
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(staged, path).unwrap();
}
