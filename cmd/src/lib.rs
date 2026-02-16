use std::ffi::{OsStr, OsString};
use std::fmt::Display;
use std::path::Path;
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::str::FromStr;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdout, Command as BaseCommand};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum CommandError {
    #[error("failed to spawn command: {command}")]
    Spawn {
        command: String,
        #[source]
        error: tokio::io::Error,
    },

    #[error("failed to get command output: {command}")]
    Output {
        command: String,
        #[source]
        error: tokio::io::Error,
    },

    #[error("command failed: {command}\n{stderr}")]
    Failure { command: String, stderr: String },

    #[error("unable to capture stdout")]
    NoStdout,

    #[error("failed to read stdout")]
    ReadStdout(#[source] tokio::io::Error),

    #[error("unable to capture stderr")]
    NoStderr,

    #[error("failed to read stderr")]
    ReadStderr(#[source] tokio::io::Error),
}

#[derive(Debug)]
pub struct Command {
    cmd: BaseCommand,
    stdout: bool,
    stderr: bool,
}

impl Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cmd = self.cmd.as_std();
        let program = cmd.get_program().to_str().unwrap();
        let args = cmd
            .get_args()
            .map(|a| a.to_str().unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        if args.is_empty() {
            write!(f, "{program}",)
        } else {
            write!(f, "{program} {args}",)
        }
    }
}

impl Command {
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Self {
            cmd: BaseCommand::new(program),
            stdout: false,
            stderr: false,
        }
    }

    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.cmd.arg(arg);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cmd.args(args);
        self
    }

    pub fn env<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.cmd.env(key, value);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.cmd.envs(vars);
        self
    }

    pub fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Command {
        self.cmd.current_dir(dir);
        self
    }

    pub fn stdout(&mut self, stdout: bool) -> &mut Command {
        self.stdout = stdout;
        self
    }

    pub fn stderr(&mut self, stderr: bool) -> &mut Command {
        self.stderr = stderr;
        self
    }

    pub fn get_stdout(&self) -> bool {
        self.stdout
    }

    pub fn get_stderr(&self) -> bool {
        self.stderr
    }

    pub fn sudo(self) -> Self {
        let mut privileged_cmd = Command::new("sudo");

        let cmd = self.cmd.as_std();

        privileged_cmd.arg("-n"); // non-interactive

        for env in cmd.get_envs() {
            if let (key, Some(value)) = env {
                let mut env_arg = OsString::new();
                env_arg.push(key);
                env_arg.push("=");
                env_arg.push(value);
                privileged_cmd.arg(env_arg);
            }
        }

        privileged_cmd
            .arg(cmd.get_program())
            .args(cmd.get_args())
            .stdout(self.get_stdout())
            .stderr(self.get_stderr());

        if let Some(dir) = cmd.get_current_dir() {
            privileged_cmd.current_dir(dir);
        }

        privileged_cmd
    }

    pub fn spawn(&mut self) -> Result<Child, CommandError> {
        self.cmd
            .stdin(Stdio::piped())
            .stdout(if self.stdout {
                Stdio::inherit()
            } else {
                Stdio::piped()
            })
            .stderr(if self.stderr {
                Stdio::inherit()
            } else {
                Stdio::piped()
            })
            .spawn()
            .map_err(|error| CommandError::Spawn {
                command: self.to_string(),
                error,
            })
    }
}

pub struct CommandOutput {
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
    pub status: Pin<Box<dyn Future<Output = Result<ExitStatus, CommandError>> + Send + 'static>>,
}

impl Command {
    pub async fn output(&mut self) -> Result<CommandOutput, CommandError> {
        // NOTE (mw): we use spawn() because output() doesn't work
        //   with stdout or stderr as we expect.
        //
        // See: https://docs.rs/tokio/latest/tokio/process/struct.Command.html#method.output
        //
        // > Note: `output()`, unlike the standard library, will unconditionally configure
        // > the stdout/stderr handles to be pipes, even if they have been previously configured.
        // > If this is not desired then the `spawn` method should be used in combination with the
        // > `wait_with_output` method on child.
        let mut child = self.spawn()?;

        let stdout = child.stdout.take().ok_or(CommandError::NoStdout)?;
        let stderr = child.stderr.take().ok_or(CommandError::NoStderr)?;

        let command_str = self.to_string();
        let status = Box::pin(async move {
            child.wait().await.map_err(|error| CommandError::Output {
                command: command_str,
                error,
            })
        });

        Ok(CommandOutput {
            stdout,
            stderr,
            status,
        })
    }

    pub async fn run(&mut self) -> Result<ExitStatus, CommandError> {
        let mut output = self.output().await?;
        let status = output.status.await?;
        if status.success() {
            Ok(status)
        } else {
            let mut stderr = String::new();
            output
                .stderr
                .read_to_string(&mut stderr)
                .await
                .map_err(CommandError::ReadStderr)?;
            Err(CommandError::Failure {
                command: self.to_string(),
                stderr,
            })
        }
    }

    pub async fn handle<OutHandler, ErrHandler, HandlerValue, HandlerError>(
        &mut self,
        stdout_handler: OutHandler,
        stderr_handler: ErrHandler,
    ) -> Result<Result<HandlerValue, HandlerError>, CommandError>
    where
        ErrHandler: Fn(&Vec<u8>) -> Result<Option<HandlerValue>, HandlerError>,
        OutHandler: Fn(&Vec<u8>) -> Result<HandlerValue, HandlerError>,
    {
        let mut output = self.output().await?;
        let status = output.status.await?;
        if status.success() {
            let mut stdout = Vec::new();
            output
                .stdout
                .read_to_end(&mut stdout)
                .await
                .map_err(CommandError::ReadStdout)?;
            return Ok(stdout_handler(&stdout));
        }

        let mut stderr = Vec::new();
        output
            .stderr
            .read_to_end(&mut stderr)
            .await
            .map_err(CommandError::ReadStderr)?;

        match stderr_handler(&stderr) {
            Err(error) => Ok(Err(error)),
            Ok(Some(value)) => Ok(Ok(value)),
            Ok(None) => Err(CommandError::Failure {
                command: self.to_string(),
                stderr: String::from_utf8_lossy(&stderr).to_string(),
            }),
        }
    }
}

impl FromStr for Command {
    type Err = shell_words::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let command_words = shell_words::split(s)?;
        if command_words.is_empty() {
            Ok(Command::new(""))
        } else {
            let mut cmd = Command::new(&command_words[0]);
            cmd.args(&command_words[1..]);
            Ok(cmd)
        }
    }
}

impl Command {
    pub fn new_sh(command: &str) -> Self {
        let mut cmd = Command::new("sh");
        cmd.arg("-c");
        cmd.arg(command);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_command() {
        assert_eq!(Command::new("lusid").to_string(), "lusid")
    }

    #[test]
    fn test_get_command_with_one_arg() {
        assert_eq!(Command::new("lusid").arg("-a").to_string(), "lusid -a")
    }

    #[test]
    fn test_get_command_with_two_args() {
        assert_eq!(
            Command::new("lusid").arg("-a").arg("-b").to_string(),
            "lusid -a -b"
        )
    }
}
