//! The OS sandbox for the agent's shell (`terminal.sandbox` in `desktop-config.json`): with
//! `low`, every command the agent runs (the `terminal` tool, its background processes and
//! `run_script`) starts with a **low-integrity** copy of the app's token. Windows' "no write up"
//! rule then keeps it from writing anything labelled Medium: nearly the whole user profile,
//! `Program Files`, other repositories. Reads are not restricted. With `off` (the default) the
//! command starts exactly as before: the same `tokio::process::Command::spawn`.
//!
//! How `low` starts a process: the app's token is duplicated, its integrity level set to Low
//! (`SetTokenInformation(TokenIntegrityLevel)`, SID `S-1-16-4096`), and the program is started
//! with `CreateProcessAsUserW`, suspended, put in its own job object (`job.rs`, the same
//! kill-the-whole-tree job as today, but assigned before the first instruction runs) and then
//! resumed. tokio's `Command` can't take a token, so the program, arguments, folder and
//! environment are read back from the same builder the `off` path spawns, the pipes are made
//! here (`CreatePipe`) and wrapped as `tokio::fs::File` (tokio's own child pipes on Windows are
//! read on blocking threads too), and the process handle is wrapped in [`LowChild`] so the
//! callers' `try_wait` / `wait` / kill-on-drop work unchanged.
//!
//! CP15.1 of `docs/backend/PHASES.md` Phase 15 stops at the token: no folder is labelled Low
//! yet (CP15.2), so under `low` a command can write only where Windows already allows Low
//! (`%USERPROFILE%\AppData\LocalLow`). The setting is read from the config record only (no
//! Settings field), as `terminal.env_passthrough` is.
//!
//! Hermes: none. Hermes has no OS sandbox for its local terminal backend
//! (`tools/environments/local.py` runs commands with the user's full rights); this is ours.
//! The low-integrity launch follows Microsoft's "Designing Applications to Run at a Low
//! Integrity Level" (duplicate the token, lower its label, `CreateProcessAsUser`); the command
//! line quoting and the environment block follow Rust's `std::process` on Windows
//! (`library/std/src/sys/process/windows.rs`, `make_command_line`, `append_arg`,
//! `make_envp`).

use std::ffi::OsStr;
use std::io;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::Command;

use crate::shell::ProcessTree;

/// What `terminal.sandbox` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sandbox {
    /// Commands run with the app's own rights (today's behaviour, the default).
    #[default]
    Off,
    /// Commands run with a low-integrity token: they can't write to Medium-labelled places.
    Low,
}

impl Sandbox {
    /// Reads the config value (`"off"` / `"low"`, any case). Anything else is `off` with a
    /// warning, so a typo never sandboxes (or un-sandboxes) silently in a surprising way.
    pub fn from_config(value: &serde_json::Value) -> Self {
        match value.as_str().map(|text| text.trim().to_ascii_lowercase()) {
            Some(text) if text == "low" => Sandbox::Low,
            Some(text) if text.is_empty() || text == "off" => Sandbox::Off,
            None if value.is_null() => Sandbox::Off,
            _ => {
                tracing::warn!("terminal.sandbox: unknown value {value}; using \"off\"");
                Sandbox::Off
            }
        }
    }
}

/// The app's setting, read at every spawn (so a change applies to the next command).
static LEVEL: AtomicU8 = AtomicU8::new(0);

/// Sets the sandbox level (the app calls it at startup and after Settings change).
pub fn set_sandbox(level: Sandbox) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

/// The sandbox level from Settings.
pub fn sandbox() -> Sandbox {
    match LEVEL.load(Ordering::Relaxed) {
        1 => Sandbox::Low,
        _ => Sandbox::Off,
    }
}

/// Our end of a child's stdin.
pub(crate) type Writer = Box<dyn AsyncWrite + Send + Unpin>;
/// Our end of a child's stdout or stderr.
pub(crate) type Reader = Box<dyn AsyncRead + Send + Unpin>;

/// A started shell process: tokio's child (`off`) or ours (`low`).
pub(crate) enum Child {
    Os(tokio::process::Child),
    #[cfg(windows)]
    Low(low::LowChild),
}

impl Child {
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self {
            Child::Os(child) => child.try_wait(),
            #[cfg(windows)]
            Child::Low(child) => child.try_wait(),
        }
    }

    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
        match self {
            Child::Os(child) => child.wait().await,
            #[cfg(windows)]
            Child::Low(child) => child.wait().await,
        }
    }
}

/// A spawned process with its pipes and its process tree (job object).
pub(crate) struct Spawned {
    pub(crate) child: Child,
    /// `Some` when spawned with `stdin_piped`.
    pub(crate) stdin: Option<Writer>,
    pub(crate) stdout: Reader,
    pub(crate) stderr: Reader,
    pub(crate) tree: ProcessTree,
}

/// How to connect the child: stdout and stderr are always piped (every caller reads them).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Spec {
    /// A pipe for stdin (else the null device).
    pub(crate) stdin_piped: bool,
    /// Kill the process (not its tree) when the [`Child`] is dropped, as tokio's `kill_on_drop`.
    pub(crate) kill_on_drop: bool,
}

/// Starts `command` (built by `shell::command`, so it starts from a cleared environment) at
/// `level`. `off` is tokio's spawn, with the same stdio and kill-on-drop calls the callers made
/// before, and the same `ProcessTree::contain`.
pub(crate) fn spawn(command: &mut Command, spec: Spec, level: Sandbox) -> io::Result<Spawned> {
    match level {
        Sandbox::Off => spawn_os(command, spec),
        #[cfg(windows)]
        Sandbox::Low => low::spawn(command, spec),
        #[cfg(not(windows))]
        Sandbox::Low => {
            tracing::warn!(
                "terminal.sandbox = low is only supported on Windows; running unsandboxed"
            );
            spawn_os(command, spec)
        }
    }
}

fn spawn_os(command: &mut Command, spec: Spec) -> io::Result<Spawned> {
    let mut child = command
        .kill_on_drop(spec.kill_on_drop)
        .stdin(if spec.stdin_piped {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let tree = ProcessTree::contain(child.id());
    let stdin = child.stdin.take().map(|pipe| Box::new(pipe) as Writer);
    let stdout = Box::new(child.stdout.take().expect("piped stdout")) as Reader;
    let stderr = Box::new(child.stderr.take().expect("piped stderr")) as Reader;
    Ok(Spawned {
        child: Child::Os(child),
        stdin,
        stdout,
        stderr,
        tree,
    })
}

/// Windows' command line for `program` and `args`, as Rust's `std::process` builds it
/// (`make_command_line` / `append_arg`): the program always quoted, an argument quoted when it
/// is empty or has a space or tab, backslashes doubled before a quote.
fn command_line<'a>(
    program: &OsStr,
    args: impl Iterator<Item = &'a OsStr>,
) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt as _;
    let no_nul = |text: &OsStr| {
        if text.encode_wide().any(|c| c == 0) {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nul byte found in provided data",
            ))
        } else {
            Ok(())
        }
    };
    no_nul(program)?;
    if program.encode_wide().any(|c| c == u16::from(b'"')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the program name contains a quote",
        ));
    }
    let mut line: Vec<u16> = vec![u16::from(b'"')];
    line.extend(program.encode_wide());
    line.push(u16::from(b'"'));
    for arg in args {
        no_nul(arg)?;
        line.push(u16::from(b' '));
        let quote = arg.is_empty()
            || arg
                .encode_wide()
                .any(|c| c == u16::from(b' ') || c == u16::from(b'\t'));
        if quote {
            line.push(u16::from(b'"'));
        }
        let mut backslashes = 0usize;
        for c in arg.encode_wide() {
            if c == u16::from(b'\\') {
                backslashes += 1;
            } else {
                if c == u16::from(b'"') {
                    line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes + 1));
                }
                backslashes = 0;
            }
            line.push(c);
        }
        if quote {
            line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
            line.push(u16::from(b'"'));
        }
    }
    line.push(0);
    Ok(line)
}

/// A `CREATE_UNICODE_ENVIRONMENT` block: `NAME=value\0` sorted by name without regard to case
/// (what `CreateProcess` expects, and what `std` sends), ending in an extra `\0`. Variables
/// the builder removed (`env_remove`) are left out.
fn environment_block<'a>(
    vars: impl Iterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt as _;
    let mut vars: Vec<(&OsStr, &OsStr)> = vars
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .collect();
    let upper = |name: &OsStr| -> Vec<u16> {
        name.encode_wide()
            .map(|c| {
                if (u16::from(b'a')..=u16::from(b'z')).contains(&c) {
                    c - 32
                } else {
                    c
                }
            })
            .collect()
    };
    vars.sort_by_key(|(name, _)| upper(name));
    let mut block = Vec::new();
    for (name, value) in vars {
        if name.is_empty()
            || name.encode_wide().skip(1).any(|c| c == u16::from(b'='))
            || name
                .encode_wide()
                .chain(value.encode_wide())
                .any(|c| c == 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid environment variable {}", name.to_string_lossy()),
            ));
        }
        block.extend(name.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// The program to start, as `std` resolves it: a path as given (`name.exe` first when it has no
/// extension, then the name as is), a bare name looked up in the child's `PATH` and then in
/// `System32` (with `.exe` added when it has no extension, and only that: an extensionless
/// script beside it in Git's `usr\bin` is not a program). `std` looks in the child's `PATH`
/// first too when the environment was changed, which every shell builder does.
fn resolve_program(program: &OsStr, path_var: Option<&OsStr>) -> io::Result<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    let exe = |path: &Path| -> PathBuf {
        if path.extension().is_none() {
            path.with_extension("exe")
        } else {
            path.to_path_buf()
        }
    };
    let with_exe = |path: PathBuf| -> Option<PathBuf> {
        let exe = exe(&path);
        if exe.is_file() {
            Some(exe)
        } else if path.is_file() {
            Some(path)
        } else {
            None
        }
    };
    let given = Path::new(program);
    if given.is_absolute() || given.components().count() > 1 {
        return with_exe(given.to_path_buf()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("program not found: {}", given.display()),
            )
        });
    }
    let system32 = std::env::var_os("SystemRoot").map(|root| PathBuf::from(root).join("System32"));
    path_var
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|dir| dir.is_absolute())
        .chain(system32)
        .map(|dir| exe(&dir.join(given)))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("program not found: {}", given.display()),
            )
        })
}

#[cfg(windows)]
mod low {
    //! The `low` spawn: Win32 calls, each `unsafe` block one call with what it relies on.

    use std::io;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use std::os::windows::process::ExitStatusExt as _;
    use std::process::ExitStatus;
    use std::sync::Arc;

    use tokio::process::Command;
    use windows::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_FAILED, WAIT_OBJECT_0,
    };
    use windows::Win32::Security::{
        CreateWellKnownSid, DuplicateTokenEx, PSID, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES,
        SecurityImpersonation, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY,
        TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel, TokenPrimary,
        WinLowLabelSid,
    };
    use windows::Win32::System::Pipes::CreatePipe;
    use windows::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
        GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
        LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
        UpdateProcThreadAttribute, WaitForSingleObject,
    };
    use windows::core::{PCWSTR, PWSTR};

    use super::{Reader, Spawned, Spec, Writer};
    use crate::job::Job;
    use crate::shell::ProcessTree;

    /// `SE_GROUP_INTEGRITY` (winnt.h): the attribute that marks a SID as the token's label.
    const SE_GROUP_INTEGRITY: u32 = 0x20;

    fn raw(handle: &OwnedHandle) -> HANDLE {
        HANDLE(handle.as_raw_handle())
    }

    /// Takes ownership of a handle a Win32 call just returned, so it closes on every path.
    fn own(handle: HANDLE) -> OwnedHandle {
        // SAFETY: called only with a handle the call before it returned to us, open and not
        // owned by anything else; from here `OwnedHandle` closes it exactly once.
        unsafe { OwnedHandle::from_raw_handle(handle.0) }
    }

    /// A primary token like the app's, labelled Low.
    pub(super) fn low_integrity_token() -> io::Result<OwnedHandle> {
        let mut current = HANDLE::default();
        // SAFETY: `GetCurrentProcess` is a pseudo-handle needing no close; `current` is a valid
        // out-pointer.
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY,
                &mut current,
            )
        }?;
        let current = own(current);
        let mut copy = HANDLE::default();
        // SAFETY: `current` is an open token with TOKEN_DUPLICATE; `copy` a valid out-pointer.
        unsafe {
            DuplicateTokenEx(
                raw(&current),
                TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
                None,
                SecurityImpersonation,
                TokenPrimary,
                &mut copy,
            )
        }?;
        let copy = own(copy);
        let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut sid_len = sid.len() as u32;
        let psid = PSID(sid.as_mut_ptr().cast());
        // SAFETY: `sid` is a buffer of `sid_len` bytes (the largest SID there is).
        unsafe { CreateWellKnownSid(WinLowLabelSid, None, Some(psid), &mut sid_len) }?;
        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES {
                Sid: psid,
                Attributes: SE_GROUP_INTEGRITY,
            },
        };
        // SAFETY: `copy` is open with TOKEN_ADJUST_DEFAULT; `label` points into `sid`, which
        // outlives the call; the length is the struct plus the SID, as the API documents.
        unsafe {
            SetTokenInformation(
                raw(&copy),
                TokenIntegrityLevel,
                (&raw const label).cast(),
                (std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32) + sid_len,
            )
        }?;
        Ok(copy)
    }

    /// An anonymous pipe, `(read end, write end)`, neither inheritable.
    fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
        let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
        // SAFETY: two valid out-pointers; no security attributes, so not inheritable.
        unsafe { CreatePipe(&mut read, &mut write, None, 0) }?;
        Ok((own(read), own(write)))
    }

    /// Lets a handle be inherited by the next process created with handle inheritance.
    fn inheritable(handle: &OwnedHandle) -> io::Result<()> {
        // SAFETY: `handle` is open and ours.
        unsafe { SetHandleInformation(raw(handle), HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }?;
        Ok(())
    }

    /// A `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`: the child inherits these handles and no others
    /// (not another tool's pipe that happens to be inheritable at that moment). Deleted on drop.
    struct HandleList {
        /// The list's memory (`usize` for alignment).
        buffer: Vec<usize>,
        /// The attribute points at this array, so it lives as long as the list.
        handles: Vec<HANDLE>,
        /// `InitializeProcThreadAttributeList` succeeded: `Drop` deletes the list.
        initialised: bool,
    }

    impl HandleList {
        fn new(handles: Vec<HANDLE>) -> io::Result<Self> {
            let mut size = 0usize;
            // SAFETY: the size query: no list, a valid out-pointer. It fails with
            // ERROR_INSUFFICIENT_BUFFER by design, so its result is not checked.
            let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
            let mut list = Self {
                buffer: vec![0usize; size.div_ceil(std::mem::size_of::<usize>())],
                handles,
                initialised: false,
            };
            let pointer = list.as_ptr();
            // SAFETY: `buffer` holds at least `size` bytes, aligned for the list.
            unsafe { InitializeProcThreadAttributeList(Some(pointer), 1, None, &mut size) }?;
            list.initialised = true;
            // SAFETY: the list is initialised; the value is the `handles` array, which lives
            // in the list and doesn't move (a `Vec`'s heap block) until the list is dropped.
            unsafe {
                UpdateProcThreadAttribute(
                    pointer,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    Some(list.handles.as_ptr().cast()),
                    std::mem::size_of_val(list.handles.as_slice()),
                    None,
                    None,
                )
            }?;
            Ok(list)
        }

        fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            LPPROC_THREAD_ATTRIBUTE_LIST(self.buffer.as_mut_ptr().cast())
        }
    }

    impl Drop for HandleList {
        fn drop(&mut self) {
            if self.initialised {
                // SAFETY: initialised by `InitializeProcThreadAttributeList`, deleted once.
                unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
            }
        }
    }

    /// A process started suspended: terminated on drop unless it was resumed, so no error
    /// after `CreateProcessAsUserW` leaves a frozen process behind.
    struct Suspended {
        process: Arc<OwnedHandle>,
        thread: OwnedHandle,
        resumed: bool,
    }

    impl Drop for Suspended {
        fn drop(&mut self) {
            if !self.resumed {
                // SAFETY: our open process handle (PROCESS_ALL_ACCESS from CreateProcess).
                let _ = unsafe { TerminateProcess(raw(&self.process), 1) };
            }
        }
    }

    pub(super) fn spawn(command: &mut Command, spec: Spec) -> io::Result<Spawned> {
        let builder = command.as_std();
        let path_var = builder
            .get_envs()
            .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
            .and_then(|(_, value)| value);
        let program = super::resolve_program(builder.get_program(), path_var)?;
        let mut line = super::command_line(builder.get_program(), builder.get_args())?;
        let environment = super::environment_block(builder.get_envs())?;
        let cwd: Option<Vec<u16>> = builder.get_current_dir().map(|dir| {
            use std::os::windows::ffi::OsStrExt as _;
            dir.as_os_str().encode_wide().chain([0]).collect()
        });
        let application: Vec<u16> = {
            use std::os::windows::ffi::OsStrExt as _;
            program.as_os_str().encode_wide().chain([0]).collect()
        };

        let token = low_integrity_token()?;
        // The child's ends and ours. stdin without a pipe is the null device, as `Stdio::null`.
        let (child_in, our_in) = if spec.stdin_piped {
            let (read, write) = pipe()?;
            (read, Some(write))
        } else {
            let null = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("NUL")?;
            (OwnedHandle::from(null), None)
        };
        let (our_out, child_out) = pipe()?;
        let (our_err, child_err) = pipe()?;
        let mut list = HandleList::new(vec![raw(&child_in), raw(&child_out), raw(&child_err)])?;

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = raw(&child_in);
        startup.StartupInfo.hStdOutput = raw(&child_out);
        startup.StartupInfo.hStdError = raw(&child_err);
        startup.lpAttributeList = list.as_ptr();

        // Inheritable only now, and closed right after the call: `std`'s own spawns (which
        // inherit every inheritable handle) can't pick these up except in this short window.
        for handle in [&child_in, &child_out, &child_err] {
            inheritable(handle)?;
        }
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: every pointer is to a live local: the token is open with the rights the call
        // needs, `application`, `cwd` and `environment` are NUL-terminated (the block double),
        // `line` is a writable NUL-terminated buffer, `startup` is a STARTUPINFOEXW whose `cb`
        // says so and whose attribute list and std handles are alive until after the call.
        let created = unsafe {
            CreateProcessAsUserW(
                Some(raw(&token)),
                PCWSTR(application.as_ptr()),
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                true,
                CREATE_NO_WINDOW
                    | CREATE_UNICODE_ENVIRONMENT
                    | CREATE_SUSPENDED
                    | EXTENDED_STARTUPINFO_PRESENT,
                Some(environment.as_ptr().cast()),
                cwd.as_ref()
                    .map_or(PCWSTR::null(), |cwd| PCWSTR(cwd.as_ptr())),
                &startup.StartupInfo,
                &mut info,
            )
        };
        // The child has its copies (or there is no child): ours of its ends go now.
        drop((child_in, child_out, child_err, list, token));
        created?;
        let mut suspended = Suspended {
            process: Arc::new(own(info.hProcess)),
            thread: own(info.hThread),
            resumed: false,
        };
        let pid = info.dwProcessId;

        // Into the job before it runs a single instruction: nothing it starts can escape.
        let job =
            Job::new().and_then(|job| job.assign_handle(raw(&suspended.process)).map(|()| job));
        let job = match job {
            Ok(job) => Some(job),
            Err(error) => {
                tracing::warn!("could not put process {pid} in a job object: {error}");
                None
            }
        };
        // SAFETY: our open thread handle of the suspended main thread.
        if unsafe { ResumeThread(raw(&suspended.thread)) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        suspended.resumed = true;
        let process = suspended.process.clone();

        let file = |handle: OwnedHandle| tokio::fs::File::from_std(std::fs::File::from(handle));
        Ok(Spawned {
            child: super::Child::Low(LowChild {
                process,
                status: None,
                waiter: None,
                kill_on_drop: spec.kill_on_drop,
            }),
            stdin: our_in.map(|handle| Box::new(file(handle)) as Writer),
            stdout: Box::new(file(our_out)) as Reader,
            stderr: Box::new(file(our_err)) as Reader,
            tree: ProcessTree::with_job(pid, job),
        })
    }

    /// The `low` process: `try_wait` / `wait` / kill-on-drop as tokio's `Child` has them.
    pub(crate) struct LowChild {
        process: Arc<OwnedHandle>,
        status: Option<ExitStatus>,
        /// One blocking wait per process, kept across `wait` calls (a caller may drop a `wait`
        /// future in a `select!` and wait again later).
        waiter: Option<tokio::task::JoinHandle<io::Result<u32>>>,
        kill_on_drop: bool,
    }

    fn exit_code(process: &OwnedHandle) -> io::Result<u32> {
        let mut code = 0u32;
        // SAFETY: an open process handle and a valid out-pointer.
        unsafe { GetExitCodeProcess(raw(process), &mut code) }?;
        Ok(code)
    }

    impl LowChild {
        pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            if self.status.is_some() {
                return Ok(self.status);
            }
            // SAFETY: an open process handle; a zero timeout only polls.
            match unsafe { WaitForSingleObject(raw(&self.process), 0) } {
                WAIT_OBJECT_0 => {}
                WAIT_FAILED => return Err(io::Error::last_os_error()),
                _ => return Ok(None),
            }
            let status = ExitStatus::from_raw(exit_code(&self.process)?);
            self.status = Some(status);
            Ok(Some(status))
        }

        pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
            if let Some(status) = self.status {
                return Ok(status);
            }
            let waiter = self.waiter.get_or_insert_with(|| {
                let process = self.process.clone();
                tokio::task::spawn_blocking(move || {
                    // SAFETY: an open process handle, kept open by the `Arc` for the call.
                    let _ = unsafe { WaitForSingleObject(raw(&process), INFINITE) };
                    exit_code(&process)
                })
            });
            let code = waiter.await.map_err(io::Error::other);
            self.waiter = None;
            let status = ExitStatus::from_raw(code??);
            self.status = Some(status);
            Ok(status)
        }
    }

    impl Drop for LowChild {
        fn drop(&mut self) {
            if self.kill_on_drop && matches!(self.try_wait(), Ok(None)) {
                // SAFETY: our open process handle.
                let _ = unsafe { TerminateProcess(raw(&self.process), 1) };
            }
        }
    }
}

#[cfg(test)]
#[path = "sandbox_tests.rs"]
mod tests;
