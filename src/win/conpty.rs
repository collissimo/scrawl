//! A child process in a Windows pseudo console (ConPTY): its terminal output comes out of a pipe
//! as VT sequences and keystrokes go in through another, with no console window. Every child
//! joins a kill-on-close job object, so it (and anything it starts) ends with Scrawl.

use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT};
use windows::Win32::System::Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

/// A running child in a pseudo console. Dropping it ends the child and closes the console.
pub struct Pty {
    process: HANDLE,
    console: HPCON,
    input: Mutex<File>,
    pub pid: u32,
}

// SAFETY: the process and console handles are only used by calls that are safe from any thread;
// the input pipe is behind a mutex.
unsafe impl Send for Pty {}
unsafe impl Sync for Pty {}

impl Pty {
    /// Starts `program` with `args` in a `cols`×`rows` pseudo console. Returns the child and the
    /// pipe its screen output arrives on, which must be read continuously until it ends.
    pub fn spawn(
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &[(String, String)],
        rows: u16,
        cols: u16,
    ) -> Result<(Self, File)> {
        let job = job()?;
        // SAFETY: plain Win32 calls on handles this function owns; each one is checked, and the
        // child-side pipe ends are closed once the console holds its own copies.
        unsafe {
            let (mut in_read, mut in_write) = (HANDLE::default(), HANDLE::default());
            let (mut out_read, mut out_write) = (HANDLE::default(), HANDLE::default());
            CreatePipe(&mut in_read, &mut in_write, None, 0).context("CreatePipe (input)")?;
            CreatePipe(&mut out_read, &mut out_write, None, 0).context("CreatePipe (output)")?;
            let size = COORD {
                X: cols as i16,
                Y: rows as i16,
            };
            let console = CreatePseudoConsole(size, in_read, out_write, 0);
            let _ = CloseHandle(in_read);
            let _ = CloseHandle(out_write);
            let input = File::from_raw_handle(in_write.0);
            let output = File::from_raw_handle(out_read.0);
            let console = console.context("CreatePseudoConsole")?;

            let mut list_size = 0_usize;
            let _ = InitializeProcThreadAttributeList(None, 1, None, &mut list_size);
            let mut list_buffer = vec![0_u8; list_size];
            let list = LPPROC_THREAD_ATTRIBUTE_LIST(list_buffer.as_mut_ptr().cast());
            let created = (|| -> Result<PROCESS_INFORMATION> {
                InitializeProcThreadAttributeList(Some(list), 1, None, &mut list_size)
                    .context("InitializeProcThreadAttributeList")?;
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    Some(console.0 as *const c_void),
                    std::mem::size_of::<HPCON>(),
                    None,
                    None,
                )
                .context("UpdateProcThreadAttribute")?;
                let mut startup = STARTUPINFOEXW::default();
                startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
                // Without this a child of a process with redirected stdio writes to those pipes
                // instead of the pseudo console.
                startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
                startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
                startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
                startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
                startup.lpAttributeList = list;
                let mut command_line = super::wide(&command_line(program, args));
                let environment = environment_block(env);
                let cwd = super::wide(&cwd.to_string_lossy());
                let mut info = PROCESS_INFORMATION::default();
                CreateProcessW(
                    PCWSTR::null(),
                    Some(PWSTR(command_line.as_mut_ptr())),
                    None,
                    None,
                    false,
                    EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
                    Some(environment.as_ptr().cast()),
                    PCWSTR(cwd.as_ptr()),
                    &startup.StartupInfo,
                    &mut info,
                )
                .with_context(|| format!("Could not start {}", program.display()))?;
                Ok(info)
            })();
            DeleteProcThreadAttributeList(list);
            let info = match created {
                Ok(info) => info,
                Err(error) => {
                    ClosePseudoConsole(console);
                    return Err(error);
                }
            };
            if let Err(error) = AssignProcessToJobObject(job, info.hProcess) {
                crate::capture::scrawl_log(format!("ConPTY: could not join the job: {error}"));
            }
            ResumeThread(info.hThread);
            let _ = CloseHandle(info.hThread);
            Ok((
                Self {
                    process: info.hProcess,
                    console,
                    input: Mutex::new(input),
                    pid: info.dwProcessId,
                },
                output,
            ))
        }
    }

    /// Types `bytes` into the console, as keystrokes.
    pub fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut input = self
            .input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        input.write_all(bytes)?;
        input.flush()
    }

    pub fn is_alive(&self) -> bool {
        // SAFETY: `process` is a live handle owned by `self`.
        unsafe { WaitForSingleObject(self.process, 0) == WAIT_TIMEOUT }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // SAFETY: the handles are owned by `self` and not used after this.
        unsafe {
            let _ = TerminateProcess(self.process, 1);
            let _ = WaitForSingleObject(self.process, 2_000);
            // The output pipe's reader keeps draining, so closing the console can't block on it.
            ClosePseudoConsole(self.console);
            let _ = CloseHandle(self.process);
        }
    }
}

/// The process-wide job every pseudo console child joins. Its handle is never closed: Windows
/// closes it when Scrawl exits, which ends the children.
fn job() -> Result<HANDLE> {
    static JOB: OnceLock<std::result::Result<usize, String>> = OnceLock::new();
    let job = JOB.get_or_init(|| {
        // SAFETY: creates and configures a job object this process keeps for its lifetime.
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).map_err(|e| e.to_string())?;
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(|e| e.to_string())?;
            Ok(job.0 as usize)
        }
    });
    match job {
        Ok(handle) => Ok(HANDLE(*handle as *mut c_void)),
        Err(error) => anyhow::bail!("Could not create the job object: {error}"),
    }
}

/// One argument quoted as the C runtime's command-line parser reads it back.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

pub fn command_line(program: &Path, args: &[String]) -> String {
    std::iter::once(quote(&program.to_string_lossy()))
        .chain(args.iter().map(|arg| quote(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `KEY=value\0…\0\0` in UTF-16, sorted case-insensitively as Windows expects.
fn environment_block(env: &[(String, String)]) -> Vec<u16> {
    let mut pairs: Vec<&(String, String)> = env.iter().collect();
    pairs.sort_by_key(|(key, _)| key.to_uppercase());
    let mut block = Vec::new();
    for (key, value) in pairs {
        block.extend(format!("{key}={value}").encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_quoted_as_the_c_runtime_reads_them() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote(r"C:\Program Files\x"), r#""C:\Program Files\x""#);
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r"dir with space\"), r#""dir with space\\""#);
        assert_eq!(
            command_line(Path::new("claude.exe"), &["--tools".into(), String::new()]),
            r#"claude.exe --tools """#
        );
    }

    #[test]
    fn the_environment_block_is_sorted_and_double_terminated() {
        let block = environment_block(&[("b".into(), "2".into()), ("A".into(), "1".into())]);
        assert_eq!(String::from_utf16_lossy(&block), "A=1\0b=2\0\0");
        assert_eq!(environment_block(&[]), vec![0, 0]);
    }
}
