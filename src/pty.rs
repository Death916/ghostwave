//! PTY handling: forkpty + non-blocking master fd (adapted from ghostling-rs).

#![allow(unsafe_code)]

use std::{
    os::fd::{AsRawFd, IntoRawFd, OwnedFd},
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
                        let s = match std::env::var_os("SHELL") {
                            Some(s) if !s.is_empty() => PathBuf::from(s),
                            _ => match unistd::User::from_uid(unistd::getuid()) {
                                Ok(Some(user)) => user.shell,
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

    /// Drain available output and feed it into the terminal.
    /// Returns Err(EndOfStream) on EOF/EIO, Ok otherwise.
    pub fn read(&self, term: &mut libghostty_vt::Terminal<'static, 'static>) -> Result<(), PtyError> {
        let mut buf = [0u8; 8192];
        loop {
            match nix::unistd::read(&self.0, &mut buf) {
                Ok(0) => return Err(PtyError::EndOfStream),
                Ok(len) => term.vt_write(&buf[..len]),
                Err(Errno::EAGAIN) => return Ok(()),
                Err(Errno::EINTR) => continue,
                Err(Errno::EIO) => return Err(PtyError::EndOfStream),
                Err(err) => return Err(PtyError::OtherError(err)),
            };
        }
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
    /// WNOHANG reap; transitions Exited -> Reaped. Call each frame.
    pub fn poll_reap(&mut self) {
        if let Child::Exited(pid) = *self {
            if let Ok(wp) = wait::waitpid(pid, Some(wait::WaitPidFlag::WNOHANG)) {
                *self = Child::Reaped(wp);
            }
        }
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
