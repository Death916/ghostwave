//! ghostd CLI — ghostwave remote session daemon.
//!
//! See the `ghostd` library for the shared machinery.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        atomic::Ordering,
        mpsc,
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use nix::{
    errno::Errno,
    sys::{
        signal::{self, Signal},
        wait::{self, WaitPidFlag},
    },
    unistd::Pid,
};

use ghostd::{
    daemonize, dcs_size_msg, die, fd_poll, fd_read, install_term_handler, log_path, pid_alive,
    pid_path, set_nonblock, set_term_size, socket_path, spawn_child, state_dir, term_size,
    truncate_log_front, write_all_blocking, DEFAULT_FIRST_ATTACH_SECS, DEFAULT_LOG_CAP_MB,
    TERM_REQUESTED, VERSION,
};

fn eprintln_exit(msg: &str) -> ! {
    eprintln!("ghostd: {msg}");
    std::process::exit(1);
}

/// Accept loop. `tx` is where pty output is published for the client.
fn accept_loop(listener: UnixListener, name: &str, log_cap: u64, first_attach_secs: u64) {
    let log_file = log_path(name);
    let pty_fd = spawn_child(&[]);
    let _ = fs::write(pid_path(name), format!("{}", nix::unistd::getpid()));
    let mut log = OpenOptions::new().create(true).append(true).open(&log_file).unwrap_or_else(|e| die(&format!("log: {e}")));
    let mut log_bytes = fs::metadata(&log_file).map(|m| m.len()).unwrap_or(0);

    let client_tx: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>> = Arc::new(Mutex::new(None));
    let started = Instant::now();
    let mut ever_attached = false;

    // Main loop: drain the pty (always — even with no client, so output is
    // logged and children never stall), accept clients.
    let mut pty_read_buf = [0u8; 32768];
    loop {
        if TERM_REQUESTED.load(Ordering::SeqCst) {
            break;
        }
        // First-attach safeguard: a session nobody ever joined goes away.
        if !ever_attached && started.elapsed().as_secs() >= first_attach_secs {
            break;
        }
        let ready = match fd_poll(&mut [(pty_fd, true), (listener.as_raw_fd(), true)], 200) {
            Ok(r) => r,
            Err(Errno::EINTR) => continue,
            Err(_) => break,
        };
        if ready.contains(&1) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    if ever_attached && client_tx.lock().unwrap().is_some() {
                        let _ = stream.write_all(b"busy: another client is attached\n");
                    } else {
                        ever_attached = true;
                        let tx = client_tx.clone();
                        let log_file = log_file.clone();
                        std::thread::spawn(move || handle_client(stream, &log_file, pty_fd, tx));
                    }
                }
                Err(_) => {}
            }
        }
        if ready.contains(&0) {
            match fd_read(pty_fd, &mut pty_read_buf) {
                Ok(0) | Err(Errno::EIO) => {
                    // Session program exited: session is over.
                    break;
                }
                Ok(n) => {
                    let chunk = &pty_read_buf[..n];
                    log_bytes += chunk.len() as u64;
                    if log_bytes > log_cap {
                        let _ = log.flush();
                        truncate_log_front(&log_file, log_cap);
                        log_bytes = log_cap;
                        log = OpenOptions::new().create(true).append(true).open(&log_file).unwrap_or(log);
                    }
                    let _ = log.write_all(chunk);
                    if let Some(tx) = client_tx.lock().unwrap().as_ref() {
                        let _ = tx.send(chunk.to_vec());
                    }
                }
                Err(Errno::EAGAIN) => {}
                Err(_) => break,
            }
        }
        // Reap the child.
        _ = wait::waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG));
    }

    // Cleanup: kill the child (if still alive), remove state.
    _ = signal::kill(Pid::from_raw(-1), Signal::SIGTERM);
    let _ = wait::waitpid(Pid::from_raw(-1), None);
    let _ = fs::remove_file(socket_path(name));
    let _ = fs::remove_file(pid_path(name));
}

/// One attached client: apply size, replay the log, then live relay.
fn handle_client(
    mut stream: UnixStream,
    log_file: &Path,
    pty_fd: i32,
    tx: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
) {
    set_nonblock(stream.as_raw_fd());
    // Blocking dup for writes to the pty (client thread only).
    let pty_write = unsafe { nix::libc::dup(pty_fd) }.max(0);

    // 1. SIZE line.
    let mut first_line = Vec::new();
    let mut byte = [0u8; 1];
    let mut got_size = false;
    while first_line.len() < 128 {
        match fd_read(stream.as_raw_fd(), &mut byte) {
            Ok(0) | Err(Errno::EAGAIN) => break,
            Ok(_) => {
                first_line.push(byte[0]);
                if byte[0] == b'\n' {
                    got_size = true;
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if !got_size {
        return;
    }
    let line = String::from_utf8_lossy(&first_line);
    if let Some(rest) = line.strip_prefix("SIZE ") {
        let mut it = rest.trim().split(' ');
        if let (Some(c), Some(r)) = (it.next(), it.next()) {
            if let (Ok(c), Ok(r)) = (c.parse::<u16>(), r.parse::<u16>()) {
                set_term_size(pty_write, c, r);
            }
        }
    } else {
        return;
    }

    // 2. Replay the log (history continuity, including the drop-gap).
    if let Ok(mut f) = File::open(log_file) {
        let mut buf = [0u8; 32768];
        loop {
            match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if stream.write_all(&buf[..n]).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
    }

    // 3. Live relay. Publish our sender for the main loop.
    let (txc, rxc) = mpsc::channel::<Vec<u8>>();
    *tx.lock().unwrap() = Some(txc);

    let mut pty_out: Vec<u8> = Vec::new(); // pending bytes to the pty (DCS-stripped)
    let mut client_out: Vec<u8> = Vec::new(); // pending bytes to the client
    let mut in_dcs = false;
    let mut dcs_buf: Vec<u8> = Vec::new();
    let mut read_buf = [0u8; 32768];

    loop {
        let ready = match fd_poll(&mut [(stream.as_raw_fd(), true)], 100) {
            Ok(r) => r,
            Err(Errno::EINTR) => continue,
            Err(_) => break,
        };
        let stream_ready = ready.contains(&0);

        // Client -> pty (strip DCS size messages).
        if stream_ready {
            match fd_read(stream.as_raw_fd(), &mut read_buf) {
                Ok(0) | Err(Errno::EAGAIN) => {}
                Ok(n) => {
                    for &b in &read_buf[..n] {
                        if in_dcs {
                            dcs_buf.push(b);
                            if dcs_buf.len() > 64 {
                                in_dcs = false;
                                dcs_buf.clear();
                            } else if b == b'\\' && dcs_buf.len() >= 2 && dcs_buf[dcs_buf.len() - 2] == 0x1b {
                                // complete DCS: \x1bPgw-size <c> <r>\x1b\
                                if let Some(rest) = dcs_buf.strip_prefix(b"Pgw-size ") {
                                    let s = String::from_utf8_lossy(rest);
                                    let s = s.trim_end_matches('\x1b').trim();
                                    let mut it = s.split(' ');
                                    if let (Some(c), Some(r)) = (it.next(), it.next()) {
                                        if let (Ok(c), Ok(r)) = (c.parse::<u16>(), r.parse::<u16>()) {
                                            set_term_size(pty_write, c, r);
                                        }
                                    }
                                }
                                in_dcs = false;
                                dcs_buf.clear();
                            }
                        } else if b == 0x1b {
                            in_dcs = true;
                            dcs_buf.clear();
                            dcs_buf.push(b);
                        } else {
                            pty_out.push(b);
                        }
                    }
                    // Flush to the pty (blocking fd: backpressure is fine).
                    if !pty_out.is_empty() {
                        let _ = write_all_blocking(pty_write, &pty_out);
                        pty_out.clear();
                    }
                }
                Err(_) => return,
            }
        }
        // Stream closed -> client gone; daemon keeps the session alive.
        if stream_ready
            && !fd_poll(&mut [(stream.as_raw_fd(), true)], 0)
                .map(|r| r.is_empty())
                .unwrap_or(false)
        {
            // POLLHUP/POLLERR without readable data: socket closed.
            let mut probe = [0u8; 1];
            if matches!(fd_read(stream.as_raw_fd(), &mut probe), Ok(0)) {
                break;
            }
        }

        // Pty -> client (drain the channel; the main loop feeds it).
        while let Ok(chunk) = rxc.try_recv() {
            client_out.extend_from_slice(&chunk);
        }
        if !client_out.is_empty() {
            match stream.write(&client_out) {
                Ok(n) => {
                    client_out.drain(..n);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        }
        // Session ended (pty EOF): send the tail, close.
        if tx.lock().unwrap().is_none() {
            while let Ok(chunk) = rxc.try_recv() {
                client_out.extend_from_slice(&chunk);
            }
            let _ = stream.write_all(&client_out);
            break;
        }
    }
    *tx.lock().unwrap() = None;
}

// ---------------------------------------------------------------------------
// commands

fn cmd_new(name: &str, cmd: &[String], log_cap: u64, first_attach_secs: u64) {
    if name.is_empty() || name.contains('/') {
        die("bad session name");
    }
    let _ = fs::create_dir_all(state_dir());
    // Liveness check: a socket that refuses connections is stale.
    let sock = socket_path(name);
    if sock.exists() {
        match UnixStream::connect(&sock) {
            Ok(_) => die(&format!("session '{name}' already exists (attach instead)")),
            Err(_) => {
                // Stale socket from a dead daemon.
                let _ = fs::remove_file(&sock);
                let _ = fs::remove_file(pid_path(name));
            }
        }
    }
    daemonize();
    install_term_handler();
    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) => die(&format!("bind {sock:?}: {e}")),
    };
    set_nonblock(listener.as_raw_fd());
    accept_loop(listener, name, log_cap, first_attach_secs);
}

fn cmd_attach(name: &str) {
    let Some(mut stream) = ghostd::connect_with_retry(name) else {
        eprintln!("ghostd: no session '{name}'");
        std::process::exit(1);
    };
    // Send our terminal size; listen for SIGWINCH and re-send as DCS.
    let stdin_fd = std::io::stdin().as_raw_fd();
    if let Some((c, r)) = term_size(stdin_fd) {
        let _ = stream.write_all(format!("SIZE {c} {r}\n").as_bytes());
    }
    let mut last_size = term_size(stdin_fd);
    extern "C" fn on_winch(_: i32) {}
    unsafe {
        nix::sys::signal::sigaction(
            nix::sys::signal::Signal::SIGWINCH,
            &nix::sys::signal::SigAction::new(
                nix::sys::signal::SigHandler::Handler(on_winch),
                nix::sys::signal::SaFlags::empty(),
                nix::sys::signal::SigSet::empty(),
            ),
        )
        .ok();
    }
    set_nonblock(stdin_fd);
    set_nonblock(stream.as_raw_fd());

    let mut in_buf = [0u8; 32768];
    let mut out_buf: Vec<u8> = Vec::new(); // pending bytes to stdout
    loop {
        // SIGWINCH: resend the size.
        let now = term_size(stdin_fd);
        if now != last_size {
            last_size = now;
            if let Some((c, r)) = now {
                let _ = stream.write_all(dcs_size_msg(c, r).as_bytes());
            }
        }
        let ready = match fd_poll(&mut [(stdin_fd, true), (stream.as_raw_fd(), true)], 100) {
            Ok(r) => r,
            Err(Errno::EINTR) => continue,
            Err(_) => break,
        };
        if ready.contains(&0) {
            match fd_read(stdin_fd, &mut in_buf) {
                Ok(0) | Err(Errno::EAGAIN) => {}
                Ok(n) => {
                    if stream.write_all(&in_buf[..n]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        if ready.contains(&1) {
            match fd_read(stream.as_raw_fd(), &mut in_buf) {
                Ok(0) | Err(Errno::EAGAIN) => {}
                Ok(n) => out_buf.extend_from_slice(&in_buf[..n]),
                Err(_) => break,
            }
            // stdout is blocking: write as much as possible now.
            let mut i = 0;
            while i < out_buf.len() {
                let n = unsafe { nix::libc::write(1, out_buf[i..].as_ptr().cast(), out_buf.len() - i) };
                if n > 0 {
                    i += n as usize;
                } else {
                    break;
                }
            }
            out_buf.drain(..i);
        }
    }
}

fn cmd_list() {
    let dir = state_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(base) = name.strip_suffix(".pid") {
            let pid = fs::read_to_string(e.path()).unwrap_or_default();
            let age = fs::metadata(socket_path(base))
                .and_then(|m| m.modified())
                .ok()
                .map(|t| t.elapsed().map(|d| d.as_secs()).unwrap_or(0));
            let log = fs::metadata(log_path(base)).map(|m| m.len()).unwrap_or(0);
            println!(
                "{base:<24} pid={} age={}s log={}B",
                pid.trim(),
                age.unwrap_or(0),
                log
            );
        }
    }
}

fn cmd_kill(name: &str) {
    let pid_file = pid_path(name);
    let Ok(pid_str) = fs::read_to_string(&pid_file) else {
        // Stale state cleanup.
        let _ = fs::remove_file(socket_path(name));
        eprintln!("ghostd: no session '{name}'");
        std::process::exit(1);
    };
    let Ok(pid) = pid_str.trim().parse::<i32>() else {
        eprintln!("ghostd: bad pid for '{name}'");
        std::process::exit(1);
    };
    let _ = signal::kill(Pid::from_raw(pid), Signal::SIGTERM);
    // Wait for cleanup (the daemon removes its own files).
    for _ in 0..50 {
        if !pid_alive(Pid::from_raw(pid)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = fs::remove_file(socket_path(name));
    let _ = fs::remove_file(pid_file);
    println!("ghostd: killed '{name}'");
}

fn cmd_kill_all() {
    let dir = state_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let mut any = false;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(base) = name.strip_suffix(".pid") {
            cmd_kill(base);
            any = true;
        }
    }
    if !any {
        println!("ghostd: no sessions");
    }
}

fn usage() -> ! {
    eprintln!(
        "ghostd {VERSION} — ghostwave remote session daemon\n\
         \n\
         usage:\n\
         \x20 ghostd new <name> [cmd...]     create a session (daemonizes)\n\
         \x20 ghostd attach <name>            attach to a session\n\
         \x20 ghostd list                     list sessions\n\
         \x20 ghostd kill <name>              kill a session\n\
         \x20 ghostd kill-all                 kill all sessions\n\
         \x20 ghostd --version\n\
         \n\
         options (new):\n\
         \x20 --first-attach-secs <n>   exit if no client attaches within n secs (default {DEFAULT_FIRST_ATTACH_SECS})\n\
         \x20 --log-cap-mb <n>          ring log cap in MiB (default {DEFAULT_LOG_CAP_MB})\n\
         \n\
         state: {:?}",
        state_dir()
    );
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    match args[0].as_str() {
        "--version" | "version" => println!("ghostd {VERSION}"),
        "new" => {
            let mut log_cap = DEFAULT_LOG_CAP_MB * 1024 * 1024;
            let mut first_attach = DEFAULT_FIRST_ATTACH_SECS;
            let mut name: Option<String> = None;
            let mut cmd: Vec<String> = Vec::new();
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--log-cap-mb" => {
                        i += 1;
                        log_cap = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_LOG_CAP_MB) * 1024 * 1024;
                    }
                    "--first-attach-secs" => {
                        i += 1;
                        first_attach = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_FIRST_ATTACH_SECS);
                    }
                    a if name.is_none() => name = Some(a.to_string()),
                    a => cmd.push(a.to_string()),
                }
                i += 1;
            }
            let Some(name) = name else { usage() };
            cmd_new(&name, &cmd, log_cap, first_attach);
        }
        "attach" => {
            let Some(name) = args.get(1) else { usage() };
            cmd_attach(name);
        }
        "list" => cmd_list(),
        "kill" => {
            let Some(name) = args.get(1) else { usage() };
            cmd_kill(name);
        }
        "kill-all" => cmd_kill_all(),
        "--help" | "-h" | "help" => usage(),
        other => eprintln_exit(&format!("unknown command: {other}")),
    }
}
