//! Local shell sessions attached to the desktop. The webview never chooses
//! the binary — every tab execs $SHELL or /bin/bash in the workspace.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::io::AsRawFd;
use std::sync::{Arc, Mutex};
use std::thread;

use tauri::{AppHandle, Emitter, State};

pub struct TermRegistry {
    sessions: Mutex<HashMap<u32, TermSession>>,
    next: Mutex<u32>,
}

struct TermSession {
    writer: Arc<Mutex<File>>,
    child: libc::pid_t,
}

impl Default for TermRegistry {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next: Mutex::new(1),
        }
    }
}

#[tauri::command]
pub fn term_open(
    app: AppHandle,
    registry: State<TermRegistry>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
) -> Result<u32, String> {
    let cwd = resolve_cwd(cwd.as_deref())?;
    let (master_fd, slave_fd) = open_pty()?;
    set_winsize(master_fd.as_raw_fd(), cols.max(20), rows.max(8))?;
    let child = unsafe { spawn_shell(slave_fd, &cwd)? };
    let master = unsafe { File::from_raw_fd(master_fd.into_raw_fd()) };
    // A PTY is full duplex. The blocking reader must have its own descriptor:
    // holding the writer mutex while `read` waits for shell output deadlocks
    // `term_write`/`term_resize`. In the Chromium build those synchronous
    // commands also occupy the hidden native relay, so one resize used to
    // stall every later desktop command until the five-minute bridge timeout.
    let reader = master
        .try_clone()
        .map_err(|error| format!("could not attach terminal output: {error}"))?;
    let writer = Arc::new(Mutex::new(master));
    let id = {
        let mut next = registry.next.lock().map_err(|_| "terminal lock")?;
        let id = *next;
        *next += 1;
        id
    };
    registry
        .sessions
        .lock()
        .map_err(|_| "terminal lock")?
        .insert(id, TermSession { writer, child });
    thread::Builder::new()
        .name(format!("phoenix-term-{id}"))
        .spawn(move || read_loop(app, id, reader))
        .map_err(|error| error.to_string())?;
    Ok(id)
}

#[tauri::command]
pub fn term_write(registry: State<TermRegistry>, id: u32, data: String) -> Result<(), String> {
    let writer = registry
        .sessions
        .lock()
        .map_err(|_| "terminal lock")?
        .get(&id)
        .map(|session| Arc::clone(&session.writer))
        .ok_or("terminal is closed")?;
    let mut writer = writer.lock().map_err(|_| "terminal lock")?;
    writer
        .write_all(data.as_bytes())
        .map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn term_resize(
    registry: State<TermRegistry>,
    id: u32,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let writer = registry
        .sessions
        .lock()
        .map_err(|_| "terminal lock")?
        .get(&id)
        .map(|session| Arc::clone(&session.writer))
        .ok_or("terminal is closed")?;
    let writer = writer.lock().map_err(|_| "terminal lock")?;
    set_winsize(writer.as_raw_fd(), cols.max(20), rows.max(8))
}

#[tauri::command]
pub fn term_close(registry: State<TermRegistry>, id: u32) -> Result<(), String> {
    let session = registry
        .sessions
        .lock()
        .map_err(|_| "terminal lock")?
        .remove(&id);
    if let Some(session) = session {
        // Close the writable master first, signal the shell's private process
        // group, and reap its leader before reporting the tab closed. Merely
        // sending SIGHUP left detached bash processes alive after the UI row
        // disappeared, so repeated tabs accumulated invisible background
        // terminals.
        drop(session.writer);
        terminate_child_bounded(session.child)?;
    }
    Ok(())
}

fn wait_for_child(child: libc::pid_t, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let mut status = 0;
        let result = unsafe { libc::waitpid(child, &mut status, libc::WNOHANG) };
        if result == child {
            return true;
        }
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                return error.raw_os_error() == Some(libc::ECHILD);
            }
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn terminate_child_bounded(child: libc::pid_t) -> Result<(), String> {
    unsafe {
        // spawn_shell creates a fresh session whose process group id is the
        // shell pid. Signal the whole tree so jobs launched from the terminal
        // cannot outlive their tab.
        libc::kill(-child, libc::SIGHUP);
        libc::kill(child, libc::SIGHUP);
    }
    if wait_for_child(child, std::time::Duration::from_millis(500)) {
        return Ok(());
    }
    unsafe {
        libc::kill(-child, libc::SIGKILL);
        libc::kill(child, libc::SIGKILL);
    }
    if wait_for_child(child, std::time::Duration::from_secs(2)) {
        Ok(())
    } else {
        Err(format!("terminal process {child} could not be reaped"))
    }
}

fn resolve_cwd(cwd: Option<&str>) -> Result<std::path::PathBuf, String> {
    let path = match cwd {
        Some(raw) if !raw.trim().is_empty() => std::path::PathBuf::from(raw),
        _ => std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("/")),
    };
    let canonical =
        std::fs::canonicalize(&path).map_err(|_| format!("folder is gone: {}", path.display()))?;
    if !canonical.is_dir() {
        return Err("choose a folder first".into());
    }
    Ok(canonical)
}

fn open_pty() -> Result<(OwnedFd, OwnedFd), String> {
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
    if master < 0 {
        return Err("could not open a terminal".into());
    }
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    if unsafe { libc::grantpt(master.as_raw_fd()) } != 0
        || unsafe { libc::unlockpt(master.as_raw_fd()) } != 0
    {
        return Err("could not grant the terminal".into());
    }
    let mut name = [0i8; 128];
    if unsafe { libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) } != 0 {
        return Err("could not name the terminal".into());
    }
    let slave = unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
    if slave < 0 {
        return Err("could not attach the shell".into());
    }
    Ok((master, unsafe { OwnedFd::from_raw_fd(slave) }))
}

unsafe fn spawn_shell(slave: OwnedFd, cwd: &std::path::Path) -> Result<libc::pid_t, String> {
    let pid = libc::fork();
    if pid < 0 {
        return Err("could not start the shell".into());
    }
    if pid > 0 {
        return Ok(pid);
    }
    // A desktop crash must not orphan an interactive shell. The parent can
    // disappear between fork and prctl, so verify it again immediately.
    let parent = libc::getppid();
    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
    if parent <= 1 || libc::getppid() != parent {
        libc::_exit(125);
    }
    let _ = libc::setsid();
    libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY, 0);
    libc::dup2(slave.as_raw_fd(), 0);
    libc::dup2(slave.as_raw_fd(), 1);
    libc::dup2(slave.as_raw_fd(), 2);
    if slave.as_raw_fd() > 2 {
        libc::close(slave.as_raw_fd());
    }
    let _ = std::env::set_current_dir(cwd);
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    let shell_c = std::ffi::CString::new(shell.as_str())
        .unwrap_or_else(|_| std::ffi::CString::new("/bin/bash").expect("bash"));
    let argv = [shell_c.as_ptr(), std::ptr::null()];
    libc::execv(shell_c.as_ptr(), argv.as_ptr());
    libc::_exit(127);
}

fn set_winsize(fd: i32, cols: u16, rows: u16) -> Result<(), String> {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &size) } != 0 {
        return Err("could not resize the terminal".into());
    }
    Ok(())
}

fn read_loop(app: AppHandle, id: u32, mut reader: File) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                let _ = app.emit("term-data", serde_json::json!({ "id": id, "data": chunk }));
            }
        }
    }
    let _ = app.emit("term-exit", serde_json::json!({ "id": id }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn pty_reader_and_writer_remain_independent() {
        let (master_fd, slave_fd) = open_pty().expect("open PTY");
        set_winsize(master_fd.as_raw_fd(), 96, 24).expect("size PTY");
        let cwd = std::env::current_dir().expect("current directory");
        let child = unsafe { spawn_shell(slave_fd, &cwd).expect("spawn shell") };
        let master = unsafe { File::from_raw_fd(master_fd.into_raw_fd()) };
        let mut reader = master.try_clone().expect("clone reader descriptor");
        let writer = Arc::new(Mutex::new(master));

        // This is the regression path from the desktop: output is waiting on
        // one descriptor while a command is written through the independently
        // locked descriptor. The old shared read mutex could never get here.
        writer
            .lock()
            .expect("writer lock")
            // Octal underscores keep the expected marker out of the echoed
            // input, so the assertion proves the shell executed the command.
            .write_all(b"printf '\\137\\137PHOENIX_PTY_OK\\137\\137\\n'\r")
            .expect("write command");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut output = Vec::new();
        while Instant::now() < deadline
            && !output.windows(18).any(|part| part == b"__PHOENIX_PTY_OK__")
        {
            let mut pollfd = libc::pollfd {
                fd: reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut pollfd, 1, 100) } <= 0 {
                continue;
            }
            let mut chunk = [0_u8; 2048];
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => output.extend_from_slice(&chunk[..read]),
            }
        }

        assert!(
            terminate_child_bounded(child).is_ok(),
            "shell child was not reaped after SIGHUP and SIGKILL"
        );
        assert!(
            output.windows(18).any(|part| part == b"__PHOENIX_PTY_OK__"),
            "shell output did not return through the PTY: {}",
            String::from_utf8_lossy(&output)
        );
    }

    #[test]
    fn closing_a_terminal_reaps_its_shell_process_group() {
        let (master_fd, slave_fd) = open_pty().expect("open PTY");
        let cwd = std::env::current_dir().expect("current directory");
        let child = unsafe { spawn_shell(slave_fd, &cwd).expect("spawn shell") };
        let mut master = unsafe { File::from_raw_fd(master_fd.into_raw_fd()) };
        master
            .write_all(b"sleep 30 &\r")
            .expect("start a background terminal job");
        master.flush().expect("flush terminal command");
        std::thread::sleep(Duration::from_millis(80));
        drop(master);
        terminate_child_bounded(child).expect("close and reap terminal tree");
        let group_probe = unsafe { libc::kill(-child, 0) };
        assert_eq!(group_probe, -1, "terminal process group survived tab close");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "unexpected process-group probe result"
        );
    }
}
