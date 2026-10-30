//! ConPTY launch attaches the suspended child to a kill-on-close Job Object before execution.
use super::{
    platform::{Control, Session},
    *,
};
use std::{
    ffi::OsStr,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{Console::*, JobObjects::*, Pipes::CreatePipe, Threading::*},
};
struct Console(HPCON);
unsafe impl Send for Console {}
impl Drop for Console {
    fn drop(&mut self) {
        unsafe { ClosePseudoConsole(self.0) }
    }
}
struct Windows {
    console: Option<Console>,
    process: OwnedHandle,
    job: OwnedHandle,
}
fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}
fn handle(raw: HANDLE) -> Result<OwnedHandle, String> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        Err(io(std::io::Error::last_os_error()))
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
}
fn pipe() -> Result<(OwnedHandle, OwnedHandle), String> {
    let (mut read, mut write) = (null_mut(), null_mut());
    if unsafe { CreatePipe(&mut read, &mut write, null(), 0) } == 0 {
        return Err(io(std::io::Error::last_os_error()));
    }
    Ok((handle(read)?, handle(write)?))
}
pub(super) fn start(
    shell: &str,
    args: &[String],
    cwd: &Path,
    size: (u16, u16),
) -> Result<Session, String> {
    let (input_read, input_write) = pipe()?;
    let (output_read, output_write) = pipe()?;
    let mut raw = 0;
    let result = unsafe {
        CreatePseudoConsole(
            COORD {
                X: size.1 as i16,
                Y: size.0 as i16,
            },
            input_read.as_raw_handle(),
            output_write.as_raw_handle(),
            0,
            &mut raw,
        )
    };
    if result < 0 {
        return Err(format!("terminal_isolation:{result}"));
    }
    let console = Console(raw);
    drop(input_read);
    drop(output_write);
    let (process, job) = spawn(shell, args, cwd, &console)?;
    Ok(Session {
        reader: Box::new(std::fs::File::from(output_read)),
        writer: Box::new(std::fs::File::from(input_write)),
        control: Box::new(Windows {
            console: Some(console),
            process,
            job,
        }),
    })
}
struct Attributes(Vec<usize>);
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.0.as_mut_ptr().cast()) }
    }
}
fn attributes(console: &Console) -> Result<Attributes, String> {
    let mut size = 0;
    unsafe {
        InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
    }
    let mut list = Attributes(vec![0; size.div_ceil(std::mem::size_of::<usize>())]);
    if unsafe { InitializeProcThreadAttributeList(list.0.as_mut_ptr().cast(), 1, 0, &mut size) }
        == 0
    {
        std::mem::forget(list);
        return Err(io(std::io::Error::last_os_error()));
    }
    if unsafe {
        UpdateProcThreadAttribute(
            list.0.as_mut_ptr().cast(),
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            console.0 as *const _,
            std::mem::size_of::<HPCON>(),
            null_mut(),
            null(),
        )
    } == 0
    {
        return Err(io(std::io::Error::last_os_error()));
    }
    Ok(list)
}
fn job() -> Result<OwnedHandle, String> {
    let job = handle(unsafe { CreateJobObjectW(null(), null()) })?;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(io(std::io::Error::last_os_error()));
    }
    Ok(job)
}
fn spawn(
    shell: &str,
    args: &[String],
    cwd: &Path,
    console: &Console,
) -> Result<(OwnedHandle, OwnedHandle), String> {
    let mut attributes = attributes(console)?;
    let job = job()?;
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of_val(&startup) as u32;
    // Null standard handles must be explicit: Windows otherwise duplicates the
    // parent's redirected pipes even with bInheritHandles=false, bypassing ConPTY.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.lpAttributeList = attributes.0.as_mut_ptr().cast();
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let mut command = wide(
        std::iter::once(shell.to_owned())
            .chain(args.iter().cloned())
            .map(|s| quote(&s))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let application = wide(shell);
    let directory = wide(cwd.as_os_str());
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            0,
            CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            null(),
            directory.as_ptr(),
            &startup.StartupInfo,
            &mut process,
        )
    } == 0
    {
        return Err(io(std::io::Error::last_os_error()));
    }
    let child = handle(process.hProcess)?;
    let thread = handle(process.hThread)?;
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0 {
        unsafe {
            TerminateProcess(child.as_raw_handle(), 1);
        }
        return Err("terminal_isolation".into());
    }
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        unsafe {
            TerminateJobObject(job.as_raw_handle(), 1);
        }
        return Err("terminal_isolation".into());
    }
    Ok((child, job))
}
fn quote(s: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for c in s.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '"' {
            result.push_str(&"\\".repeat(slashes * 2 + 1));
            result.push(c);
        } else {
            result.push_str(&"\\".repeat(slashes));
            result.push(c);
        }
        slashes = 0;
    }
    result.push_str(&"\\".repeat(slashes * 2));
    result.push('"');
    result
}
impl Control for Windows {
    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        let console = self.console.as_ref().ok_or("terminal_ended")?;
        if unsafe {
            ResizePseudoConsole(
                console.0,
                COORD {
                    X: cols as i16,
                    Y: rows as i16,
                },
            )
        } < 0
        {
            Err("terminal_resize".into())
        } else {
            Ok(())
        }
    }
    fn ended(&mut self) -> Result<bool, String> {
        Ok(unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } == WAIT_OBJECT_0)
    }
    fn terminate(&mut self) -> Result<(), String> {
        unsafe {
            if TerminateJobObject(self.job.as_raw_handle(), 1) == 0 {
                return Err(io(std::io::Error::last_os_error()));
            }
            if WaitForSingleObject(self.process.as_raw_handle(), 5000) != WAIT_OBJECT_0 {
                return Err("terminal_busy".into());
            }
        }
        drop(self.console.take());
        Ok(())
    }
}
