use super::native::{Handle, error, invoke, wide};
use std::ffi::OsString;
use std::mem::{size_of, zeroed};
use std::ptr::null;
use windows_sys::Win32::Foundation::{GetLastError, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Threading::*;

fn job() -> Result<Handle, String> {
    super::native::before("CreateJobObjectW")?;
    let job = Handle::new("CreateJobObjectW", unsafe {
        CreateJobObjectW(null(), null())
    })?;
    let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    invoke("SetInformationJobObject", || unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&information as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    })?;
    Ok(job)
}

fn startup() -> Result<STARTUPINFOW, String> {
    let mut information: STARTUPINFOW = unsafe { zeroed() };
    information.cb = size_of::<STARTUPINFOW>() as u32;
    information.dwFlags = STARTF_USESTDHANDLES;
    // Pipe handles supplied by the outer ProcessService are inheritable. There
    // is no owned job/token handle in this list, so descendants cannot keep the
    // kill-on-close job alive after the runner dies.
    information.hStdInput = std_handle(STD_INPUT_HANDLE)?;
    information.hStdOutput = std_handle(STD_OUTPUT_HANDLE)?;
    information.hStdError = std_handle(STD_ERROR_HANDLE)?;
    Ok(information)
}

fn std_handle(which: u32) -> Result<windows_sys::Win32::Foundation::HANDLE, String> {
    let handle = unsafe { GetStdHandle(which) };
    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        Err(error("GetStdHandle", unsafe { GetLastError() }))
    } else {
        Ok(handle)
    }
}

pub(super) fn run(token: &Handle, arguments: &[OsString]) -> Result<u32, String> {
    let job = job()?;
    let startup = startup()?;
    let mut command = command_line(arguments)?;
    let mut information: PROCESS_INFORMATION = unsafe { zeroed() };
    invoke("CreateProcessAsUserW", || unsafe {
        CreateProcessAsUserW(
            token.0,
            null(),
            command.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_SUSPENDED,
            null(),
            null(),
            &startup,
            &mut information,
        )
    })?;
    let process = Handle::new("CreateProcessAsUserW(process)", information.hProcess)?;
    let thread = match Handle::new("CreateProcessAsUserW(thread)", information.hThread) {
        Ok(thread) => thread,
        Err(error) => {
            terminate_suspended(&process);
            return Err(error);
        }
    };
    if let Err(error) = assign_resume(&job, &process, &thread) {
        terminate_suspended(&process);
        return Err(error);
    }
    thread.close()?;
    super::native::before("WaitForSingleObject")?;
    let waited = unsafe { WaitForSingleObject(process.0, INFINITE) };
    if waited != WAIT_OBJECT_0 {
        return Err(error("WaitForSingleObject", unsafe { GetLastError() }));
    }
    let mut exit = 0;
    invoke("GetExitCodeProcess", || unsafe {
        GetExitCodeProcess(process.0, &mut exit)
    })?;
    // The last job handle closes before returning; living descendants are killed.
    job.close()?;
    process.close()?;
    Ok(exit)
}

fn terminate_suspended(process: &Handle) {
    if let Err(cleanup) = invoke("TerminateProcess", || unsafe {
        TerminateProcess(process.0, 127)
    }) {
        eprintln!("{cleanup}");
    }
}

fn assign_resume(job: &Handle, process: &Handle, thread: &Handle) -> Result<(), String> {
    invoke("AssignProcessToJobObject", || unsafe {
        AssignProcessToJobObject(job.0, process.0)
    })?;
    super::native::before("ResumeThread")?;
    if unsafe { ResumeThread(thread.0) } == u32::MAX {
        return Err(error("ResumeThread", unsafe { GetLastError() }));
    }
    Ok(())
}

fn command_line(arguments: &[OsString]) -> Result<Vec<u16>, String> {
    let shell_tail = cmd_tail(arguments);
    let prefix = shell_tail.unwrap_or(arguments.len());
    let mut result = Vec::new();
    for (index, argument) in arguments[..prefix].iter().enumerate() {
        if shell_tail.is_some()
            && index + 1 == prefix
            && !arguments[..prefix].iter().any(|arg| {
                arg.to_str()
                    .is_some_and(|text| text.eq_ignore_ascii_case("/s"))
            })
        {
            result.extend([32, 47, 115]);
        }
        if !result.is_empty() {
            result.push(32);
        }
        let argument = wide(argument)?;
        result.extend(quote(&argument[..argument.len() - 1]));
    }
    if let Some(tail) = shell_tail {
        // cmd.exe does not use CRT escaping for its /c command tail. /s strips
        // exactly the outer pair, leaving quotes inside the script untouched.
        result.extend([32, 34]);
        for (index, argument) in arguments[tail..].iter().enumerate() {
            if index != 0 {
                result.push(32);
            }
            let argument = wide(argument)?;
            result.extend_from_slice(&argument[..argument.len() - 1]);
        }
        result.push(34);
    }
    if result.len() >= 32767 {
        return Err("windows-acl-run: command line exceeds Win32 limit".into());
    }
    result.push(0);
    Ok(result)
}

fn cmd_tail(arguments: &[OsString]) -> Option<usize> {
    let command = std::path::Path::new(arguments.first()?);
    if !command
        .file_name()?
        .to_str()?
        .eq_ignore_ascii_case("cmd.exe")
    {
        return None;
    }
    arguments
        .iter()
        .position(|argument| {
            argument.to_str().is_some_and(|text| {
                text.eq_ignore_ascii_case("/c") || text.eq_ignore_ascii_case("/k")
            })
        })
        .map(|position| position + 1)
}

// The CRT argv rules: double trailing slashes and those preceding a quote.
fn quote(argument: &[u16]) -> Vec<u16> {
    if !argument.is_empty()
        && !argument
            .iter()
            .any(|unit| matches!(unit, 9 | 10 | 13 | 32 | 34))
    {
        return argument.to_vec();
    }
    let mut result = vec![34];
    let mut slashes = 0;
    for &unit in argument {
        if unit == 92 {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            92,
            if unit == 34 { slashes * 2 + 1 } else { slashes },
        ));
        result.push(unit);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n(92, slashes * 2));
    result.push(34);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_windows_arguments_preserve_quotes_and_trailing_slashes() {
        fn quoted(input: &str) -> String {
            String::from_utf16(&quote(&input.encode_utf16().collect::<Vec<_>>())).unwrap()
        }
        assert_eq!(quoted(""), "\"\"");
        assert_eq!(quoted("a b"), "\"a b\"");
        assert_eq!(quoted("a\"b"), "\"a\\\"b\"");
        assert_eq!(quoted("a\\"), "a\\");
    }
    #[test]
    fn cmd_script_preserves_inner_path_quotes_and_redirects() {
        let arguments: Vec<OsString> = ["cmd.exe", "/d", "/c", "echo own>\"%TMP%\\own.txt\""]
            .into_iter()
            .map(OsString::from)
            .collect();
        let command = command_line(&arguments).unwrap();
        assert_eq!(
            String::from_utf16(&command[..command.len() - 1]).unwrap(),
            "cmd.exe /d /s /c \"echo own>\"%TMP%\\own.txt\"\""
        );
    }
    #[test]
    fn invalid_command_never_reaches_spawn() {
        assert!(command_line(&[OsString::from("bad\0command")]).is_err());
        assert!(command_line(&[OsString::from("x".repeat(32767))]).is_err());
    }
}
