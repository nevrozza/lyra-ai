use std::io;
use tokio::process::{Child, Command};

#[cfg(unix)]
mod platform {
    use super::*;

    pub const SHELL: &str = "/bin/sh";

    pub fn shell(script: &str) -> io::Result<Command> {
        let mut command = Command::new(SHELL);
        command
            .args(["-c", script])
            .stdin(std::process::Stdio::null())
            .process_group(0);
        Ok(command)
    }

    pub async fn start(_: &mut Child, _: &str) -> io::Result<()> {
        Ok(())
    }

    pub struct Tree(libc::pid_t);

    impl Tree {
        pub fn attach(child: &Child) -> io::Result<Self> {
            child
                .id()
                .map(|pid| Self(pid as libc::pid_t))
                .ok_or_else(|| io::Error::other("Missing command PID"))
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            // The shell has its own process group, including its children.
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{path::PathBuf, process::Stdio, ptr};
    use tokio::io::AsyncWriteExt;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
            Threading::CREATE_NO_WINDOW,
        },
    };

    pub const SHELL: &str = "Windows PowerShell";

    pub fn shell(_: &str) -> io::Result<Command> {
        let root = std::env::var_os("SystemRoot")
            .ok_or_else(|| io::Error::other("Missing Windows SystemRoot"))?;
        let exe = PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let mut command = Command::new(exe);
        // Wait for stdin before evaluating any project code. The Job Object is attached first.
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
            "[Console]::InputEncoding = [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding; & ([scriptblock]::Create([Console]::In.ReadToEnd())); $succeeded = $?; if ($LASTEXITCODE) { exit $LASTEXITCODE }; if (!$succeeded) { exit 1 }"])
            .stdin(Stdio::piped()).creation_flags(CREATE_NO_WINDOW);
        Ok(command)
    }

    pub async fn start(child: &mut Child, script: &str) -> io::Result<()> {
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(script.as_bytes()).await?;
        stdin.shutdown().await
    }

    pub struct Tree(HANDLE);

    // This unique handle is safe to close from any thread.
    unsafe impl Send for Tree {}

    impl Tree {
        pub fn attach(child: &Child) -> io::Result<Self> {
            let process = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("Missing command handle"))?;
            unsafe {
                let handle = CreateJobObjectW(ptr::null(), ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let tree = Self(handle);
                let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    ptr::from_ref(&limits).cast(),
                    size_of_val(&limits) as u32,
                ) == 0
                    || AssignProcessToJobObject(handle, process) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(tree)
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

pub(super) use platform::{SHELL, shell, start};
pub(in crate::server) use platform::Tree;
