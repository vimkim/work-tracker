use std::{
    ffi::OsStr,
    fs,
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

    pub fn database_path(&self) -> PathBuf {
        self.data.join("work-tracker/work-tracker.db")
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

pub fn assert_database_exists(path: &Path) -> Result<()> {
    ensure!(
        path.is_file(),
        "database was not created at {}",
        path.display()
    );
    Ok(())
}
