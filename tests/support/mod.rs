use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, ensure};
use tempfile::TempDir;

pub struct CliHarness {
    _root: TempDir,
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

#[allow(dead_code)]
impl CliHarness {
    pub fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let config = root.path().join("config");
        let data = root.path().join("data");
        for directory in [&home, &config, &data] {
            fs::create_dir_all(directory)?;
        }
        Ok(Self {
            _root: root,
            home,
            config,
            data,
        })
    }

    pub fn run<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Command::new(env!("CARGO_BIN_EXE_work-tracker"))
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("USER", "harness-agent")
            .output()
            .context("failed to invoke compiled work-tracker CLI")
    }

    pub fn run_with_fake_gh<I, S>(&self, fake: &FakeGh, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Command::new(env!("CARGO_BIN_EXE_work-tracker"))
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("USER", "harness-agent")
            .env("PATH", fake.bin_dir())
            .env("FAKE_GH_ROOT", fake.root())
            .output()
            .context("failed to invoke compiled work-tracker CLI")
    }

    pub fn database_path(&self) -> PathBuf {
        self.data.join("work-tracker/work-tracker.db")
    }

    pub fn config_path(&self) -> PathBuf {
        self.config.join("work-tracker/config.json")
    }

    pub fn github_cache_path(&self, owner: &str, repository: &str) -> PathBuf {
        self.data
            .join("work-tracker/github")
            .join(owner)
            .join(format!("{repository}.db"))
    }
}

#[allow(dead_code)]
pub struct FakeGh {
    _root: TempDir,
    bin: PathBuf,
    state: PathBuf,
}

#[allow(dead_code)]
impl FakeGh {
    pub fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let bin = root.path().join("bin");
        let state = root.path().join("state");
        fs::create_dir_all(&bin)?;
        fs::create_dir_all(state.join("responses"))?;
        let executable = bin.join("gh");
        fs::write(
            &executable,
            r#"#!/bin/sh
set -eu
root=$FAKE_GH_ROOT
count_file=$root/count
if [ -f "$count_file" ]; then
    count=$(/bin/cat "$count_file")
else
    count=0
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
{
    printf 'CALL'
    for argument in "$@"; do
        printf '\t%s' "$argument"
    done
    printf '\n'
} >> "$root/calls"
response=$root/responses/$count
if [ -f "$response.stdout" ]; then /bin/cat "$response.stdout"; fi
if [ -f "$response.stderr" ]; then /bin/cat "$response.stderr" >&2; fi
if [ -f "$response.code" ]; then exit "$(/bin/cat "$response.code")"; fi
exit 0
"#,
        )?;
        let mut permissions = fs::metadata(&executable)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions)?;
        Ok(Self {
            _root: root,
            bin,
            state,
        })
    }

    pub fn respond(&self, number: usize, code: i32, stdout: &str, stderr: &str) -> Result<()> {
        let response = self.state.join("responses").join(number.to_string());
        fs::write(response.with_extension("code"), code.to_string())?;
        fs::write(response.with_extension("stdout"), stdout)?;
        fs::write(response.with_extension("stderr"), stderr)?;
        Ok(())
    }

    pub fn calls(&self) -> Result<String> {
        fs::read_to_string(self.state.join("calls")).context("fake gh recorded no calls")
    }

    pub fn remove_executable(&self) -> Result<()> {
        fs::remove_file(self.bin.join("gh")).context("failed to remove fake gh executable")
    }

    fn bin_dir(&self) -> &Path {
        &self.bin
    }

    fn root(&self) -> &Path {
        &self.state
    }
}

pub fn stdout(output: &Output) -> Result<&str> {
    std::str::from_utf8(&output.stdout).context("stdout was not UTF-8")
}

pub fn stderr(output: &Output) -> Result<&str> {
    std::str::from_utf8(&output.stderr).context("stderr was not UTF-8")
}

pub fn assert_success(output: &Output) -> Result<()> {
    ensure!(
        output.status.success(),
        "command failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        stdout(output)?,
        stderr(output)?
    );
    Ok(())
}

#[allow(dead_code)]
pub fn assert_database_exists(path: &Path) -> Result<()> {
    ensure!(
        path.is_file(),
        "database was not created at {}",
        path.display()
    );
    Ok(())
}
