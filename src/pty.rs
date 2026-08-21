//! PTY handling: forkpty + non-blocking master fd (adapted from ghostling-rs).

#![allow(unsafe_code)]

use std::{
    os::fd::{AsFd, AsRawFd, IntoRawFd, OwnedFd},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::Command,
};

use nix::{
    errno::Errno,
    fcntl::{self, OFlag},
    pty::ForkptyResult,
    sys::{signal, wait},
    unistd::{self, Pid},
};

#[derive(Clone, Copy, Debug, Default)]
pub struct WinSize {
    pub cols: u16,
    pub rows: u16,
    pub xpixel: u16,
    pub ypixel: u16,
}

impl WinSize {
    pub fn to_winsize(self) -> nix::pty::Winsize {
        nix::pty::Winsize {
            ws_col: self.cols,
            ws_row: self.rows,
            ws_xpixel: self.xpixel,
            ws_ypixel: self.ypixel,
        }
    }
}

#[derive(Debug)]
pub enum PtyMsg {
    Data(Vec<u8>),
    Eof,
    Error(String),
}

/// Handle to a pseudo-terminal master.
pub struct Pty(OwnedFd);

impl Pty {
    /// Spawn a program (default: user's shell) in a new pseudo-terminal.
    /// Master fd is set non-blocking so the app loop can poll it.
    pub fn new(
        size: WinSize,
        program: Option<&str>,
        args: &[&str],
        cwd: Option<&str>,
    ) -> std::io::Result<(Self, Child)> {
        match unsafe { nix::pty::forkpty(&size.to_winsize(), None)? } {
            ForkptyResult::Child => {
                let (shell, extra_args): (PathBuf, Vec<&str>) = match program {
                    Some(p) if !p.is_empty() => (PathBuf::from(p), args.to_vec()),
                    _ => {
                        // Prefer the passwd entry: $SHELL can be overridden by
                        // wrappers (nix-shell sets it to bash regardless of the
                        // user's real login shell). Real terminals use passwd.
                        let s = match unistd::User::from_uid(unistd::getuid()) {
                            Ok(Some(user)) if !user.shell.as_os_str().is_empty() => user.shell,
                            _ => match std::env::var_os("SHELL") {
                                Some(s) if !s.is_empty() => PathBuf::from(s),
                                _ => PathBuf::from("/bin/sh"),
                            },
                        };
                        (s, vec![])
                    }
                };
                let arg0 = shell.file_name().unwrap_or(shell.as_os_str());
                let mut cmd = Command::new(&shell);
                cmd.env("TERM", "xterm-256color").arg0(arg0);
                for a in &extra_args {
                    cmd.arg(a);
                }
                if let Some(c) = cwd {
                    if !c.is_empty() {
                        cmd.current_dir(c);
                    }
                }
                _ = cmd.exec();
                std::process::exit(127);
            }
            ForkptyResult::Parent { child, master: fd } => {
                let raw_flags = fcntl::fcntl(&fd, fcntl::F_GETFL)?;
                let flags = OFlag::from_bits_retain(raw_flags) | OFlag::O_NONBLOCK;
                _ = fcntl::fcntl(&fd, fcntl::F_SETFL(flags))?;
                Ok((Self(fd), Child::Active(child)))
            }
        }
    }

    /// Drain available output into `out` (caller reuses buffer to avoid allocation churn).
    /// Returns Err(EndOfStream) on EOF/EIO, Ok(()) otherwise.
    pub fn read_into(&self, out: &mut Vec<u8>) -> Result<(), PtyError> {
        let mut buf = [0u8; 65536];
        loop {
            match nix::unistd::read(&self.0, &mut buf) {
                Ok(0) => return Err(PtyError::EndOfStream),
                Ok(len) => out.extend_from_slice(&buf[..len]),
                Err(Errno::EAGAIN) => return Ok(()),
                Err(Errno::EINTR) => continue,
                Err(Errno::EIO) => return Err(PtyError::EndOfStream),
                Err(err) => return Err(PtyError::OtherError(err)),
            };
        }
    }

    /// Drain available output; returns the raw bytes read.
    /// Returns Err(EndOfStream) on EOF/EIO, Ok(bytes) otherwise.
    pub fn read(&self) -> Result<Vec<u8>, PtyError> {
        let mut out: Vec<u8> = Vec::with_capacity(65536);
        self.read_into(&mut out)?;
        Ok(out)
    }

    /// Spawns a dedicated background thread that polls and reads from the master PTY fd.
    /// This decouples PTY I/O throughput from the UI rendering/VSync frame rate.
    pub fn spawn_reader(&self) -> std::io::Result<std::sync::mpsc::Receiver<PtyMsg>> {
        let fd = self.0.try_clone()?;
        let (tx, rx) = std::sync::mpsc::sync_channel(512);
        std::thread::Builder::new()
            .name("ghostwave-pty-reader".into())
            .spawn(move || {
                use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
                let mut buf = [0u8; 65536];
                let mut poll_fds = [PollFd::new(fd.as_fd(), PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)];
                loop {
                    match poll(&mut poll_fds, PollTimeout::NONE) {
                        Ok(_) => {
                            let revents = poll_fds[0].revents().unwrap_or(PollFlags::empty());
                            if revents.contains(PollFlags::POLLIN) {
                                match nix::unistd::read(&fd, &mut buf) {
                                    Ok(0) => {
                                        let _ = tx.send(PtyMsg::Eof);
                                        break;
                                    }
                                    Ok(len) => {
                                        if tx.send(PtyMsg::Data(buf[..len].to_vec())).is_err() {
                                            break;
                                        }
                                    }
                                    Err(Errno::EINTR) | Err(Errno::EAGAIN) => continue,
                                    Err(Errno::EIO) => {
                                        let _ = tx.send(PtyMsg::Eof);
                                        break;
                                    }
                                    Err(err) => {
                                        let _ = tx.send(PtyMsg::Error(err.to_string()));
                                        break;
                                    }
                                }
                            } else if revents.contains(PollFlags::POLLHUP) || revents.contains(PollFlags::POLLERR) {
                                let _ = tx.send(PtyMsg::Eof);
                                break;
                            }
                        }
                        Err(Errno::EINTR) => continue,
                        Err(_) => {
                            let _ = tx.send(PtyMsg::Eof);
                            break;
                        }
                    }
                }
            })?;
        Ok(rx)
    }

    /// Best-effort write; retries EINTR, advances past partial writes, drops
    /// the remainder on EAGAIN (back-pressure, like most terminals).
    pub fn write(&self, data: &[u8]) {
        let mut remaining = data;
        while !remaining.is_empty() {
            match nix::unistd::write(&self.0, remaining) {
                Ok(len) => remaining = &remaining[len..],
                Err(Errno::EINTR) => continue,
                Err(_) => break,
            }
        }
    }

    /// Duplicate the master fd as a raw int (for terminal effect callbacks
    /// that need to write responses, e.g. VT queries). Ownership transfers
    /// to the caller (the pane closes it on drop).
    pub fn dup_raw(&self) -> std::io::Result<i32> {
        let dup = nix::unistd::dup(&self.0)?;
        Ok(dup.into_raw_fd())
    }

    pub fn resize(&self, size: WinSize) {
        nix::ioctl_write_ptr_bad!(tiocswinsz, nix::libc::TIOCSWINSZ, nix::pty::Winsize);
        _ = unsafe { tiocswinsz(self.0.as_raw_fd(), &size.to_winsize()) };
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.0.as_raw_fd()
    }
}

/// Child process state with reaping.
#[derive(Debug)]
pub enum Child {
    Active(Pid),
    Exited(Pid),
    Reaped(wait::WaitStatus),
}

impl Child {
    /// WNOHANG reap; transitions Exited -> Reaped. Returns the child's exit
    /// code (128+signal for signal deaths) once it has actually exited.
    /// Call each frame.
    pub fn poll_reap(&mut self) -> Option<i32> {
        if let Child::Exited(pid) = *self {
            if let Ok(wp) = wait::waitpid(pid, Some(wait::WaitPidFlag::WNOHANG)) {
                let code = match wp {
                    wait::WaitStatus::Exited(_, c) => Some(c),
                    wait::WaitStatus::Signaled(_, sig, _) => Some(128 + sig as i32),
                    _ => None,
                };
                *self = Child::Reaped(wp);
                return code;
            }
        }
        None
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        match *self {
            Child::Active(pid) => {
                let _ = signal::kill(pid, signal::SIGHUP);
                let _ = wait::waitpid(pid, None);
            }
            Child::Exited(pid) => {
                let _ = wait::waitpid(pid, None);
            }
            Child::Reaped(_) => {}
        }
    }
}

#[derive(Clone, Debug)]
pub enum PtyError {
    EndOfStream,
    OtherError(nix::errno::Errno),
}
