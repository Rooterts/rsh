//! Process and signal management for job control.
//!
//! `rsh` implements a simplified but real job control model:
//!
//! * Every foreground command runs in its own process group, which receives
//!   the terminal (via `tcsetpgrp`) so that ^C and ^Z reach the command and
//!   not the shell.
//! * The shell itself ignores SIGTSTP (it is not a job to stop) and ignores
//!   SIGINT while it waits for a foreground child, so a ^C kills the child but
//!   leaves the shell running.
//! * The foreground `waitpid` is done with `WUNTRACED`/`WCONTINUED`, so a job
//!   suspended with ^Z is reported instead of reaped and can later be resumed
//!   with `fg`/`bg`.
//!
//! Everything here is `#[cfg(unix)]`; on other platforms rsh falls back to the
//! plain synchronous spawn handled by the executor.
#![cfg(unix)]

use std::os::unix::io::RawFd;

/// Result of waiting for a foreground child.
#[derive(Debug)]
pub enum FgOutcome {
    /// The child stopped (e.g. it received SIGTSTP via ^Z) and was not reaped,
    /// so it is kept in the job table for later `fg`/`bg`.
    Stopped,
    /// The child terminated. Holds its exit status (or `128 + signal` when it
    /// died from a signal, matching `$?` conventions).
    Exited(i32),
}

/// The shell's own process group id.
pub fn own_pgid() -> i32 {
    unsafe { libc::getpgrp() }
}

/// Gives the terminal to the given foreground process group.
pub fn give_terminal(pgid: i32) {
    unsafe {
        let _ = libc::tcsetpgrp(libc::STDIN_FILENO as RawFd, pgid);
    }
}

/// Returns the terminal to the shell's own process group.
pub fn release_terminal() {
    let pgrp = own_pgid();
    unsafe {
        let _ = libc::tcsetpgrp(libc::STDIN_FILENO as RawFd, pgrp);
    }
}

/// Puts `pid` into a new process group of its own (called from the parent;
/// errors are ignored because the child races to do the same).
pub fn set_group(pid: i32) {
    unsafe {
        let _ = libc::setpgid(pid, pid);
    }
}

/// Pre-exec setup for a foreground child: its own process group plus default
/// signal dispositions, so ^C and ^Z reach the child.
pub fn foreground_child_setup() {
    unsafe {
        let _ = libc::setpgid(0, 0);
        let _ = libc::signal(libc::SIGINT, libc::SIG_DFL);
        let _ = libc::signal(libc::SIGQUIT, libc::SIG_DFL);
        let _ = libc::signal(libc::SIGTSTP, libc::SIG_DFL);
    }
}

/// Pre-exec setup for a background child: its own process group and it ignores
/// the signals a foreground job would normally stop it from.
pub fn background_child_setup() {
    unsafe {
        let _ = libc::setpgid(0, 0);
        let _ = libc::signal(libc::SIGINT, libc::SIG_IGN);
        let _ = libc::signal(libc::SIGTSTP, libc::SIG_IGN);
    }
}

/// Shell-wide signal setup: a shell is not itself a job, so it ignores ^Z.
/// It must also ignore SIGTTOU/SIGTTIN: otherwise calling tcsetpgrp while the
/// shell is in the background (e.g. taking the terminal back after a
/// foreground child finished) would stop the shell itself with
/// "suspended (tty output)".
pub fn shell_setup() {
    unsafe {
        let _ = libc::signal(libc::SIGTSTP, libc::SIG_IGN);
        let _ = libc::signal(libc::SIGTTIN, libc::SIG_IGN);
        let _ = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
    }
}

/// The shell ignores SIGINT while it waits on a foreground child (so a ^C only
/// reaches the child). readline installs its own handler when we are back at
/// the prompt.
fn ignore_sigint_on() {
    unsafe {
        signal_dispose(libc::SIGINT, libc::SIG_IGN);
    }
}

fn ignore_sigint_off() {
    unsafe {
        signal_dispose(libc::SIGINT, libc::SIG_DFL);
    }
}

unsafe fn signal_dispose(sig: libc::c_int, handler: libc::sighandler_t) {
    unsafe {
        let _ = libc::signal(sig, handler);
    }
}

/// Sends SIGCONT to every process of a job's process group (`-pgid`) to resume
/// it. Single-process jobs have pid == pgid, so this works for both.
pub fn continue_job(pgid: i32) {
    unsafe {
        let _ = libc::kill(-pgid, libc::SIGCONT);
    }
}

/// Waits for every stage pid of a foreground pipeline (all in one process
/// group), honoring stops (^Z). Returns the last stage's exit status and
/// whether the pipeline stopped instead of running to completion.
pub fn wait_pipeline(pids: &[i32]) -> (i32, bool) {
    ignore_sigint_on();
    let mut last = 0;
    let mut stopped = false;
    for &pid in pids {
        let mut status = 0;
        let mut ret = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
        while ret < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            ret = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
        }
        if ret < 0 {
            continue; // pid already reaped by someone else
        }
        if libc::WIFSTOPPED(status) {
            stopped = true;
        } else {
            last = exit_status(status);
        }
    }
    ignore_sigint_off();
    (last, stopped)
}

/// Non-blocking reap: returns `Some(status)` if the child has been reaped,
/// `None` if it is still running (or already handled by another waiter).
pub fn try_reap(pid: i32) -> Option<i32> {
    let mut status = 0;
    let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    if ret == pid {
        Some(exit_status(status))
    } else {
        None
    }
}

/// Sends any signal to a process; returns whether the signal was delivered.
pub fn kill_pid(pid: i32, sig: i32) -> bool {
    unsafe { libc::kill(pid, sig as libc::c_int) == 0 }
}

/// Blocks until `pid` terminates and returns its exit status (or `128+signal`
/// if it died from a signal). Returns `None` if `pid` is not a known child.
pub fn wait_blocking(pid: i32) -> Option<i32> {
    let mut status = 0;
    loop {
        let ret = unsafe { libc::waitpid(pid, &mut status, 0) };
        if ret == pid {
            return Some(exit_status(status));
        }
        if ret < 0 {
            return None;
        }
        // EINTR loop; keep waiting.
    }
}

/// Blocks until `pid` terminates or stops. See [`FgOutcome`].
pub fn wait_foreground(pid: i32) -> FgOutcome {
    ignore_sigint_on();
    let outcome = loop {
        let mut status = 0;
        let ret = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED | libc::WCONTINUED) };
        if ret < 0 {
            break FgOutcome::Exited(1);
        }
        if libc::WIFSTOPPED(status) {
            break FgOutcome::Stopped;
        }
        if libc::WIFCONTINUED(status) {
            continue;
        }
        break FgOutcome::Exited(exit_status(status));
    };
    ignore_sigint_off();
    outcome
}

fn exit_status(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

#[cfg(test)]
#[allow(clippy::zombie_processes)] // children are reaped via waitpid, not Child::wait
mod tests {
    use super::*;

    fn spawn_sh(args: &[&str]) -> std::process::Child {
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new("sh");
        cmd.args(args);
        // Give the child its own process group before exec, exactly how the
        // executor does it for real jobs (setpgid from the parent races with
        // exec and can fail with EACCES).
        unsafe {
            cmd.pre_exec(|| {
                let _ = libc::setpgid(0, 0);
                Ok(())
            });
        }
        cmd.spawn().unwrap()
    }

    #[test]
    fn test_wait_foreground_exit_code() {
        let child = spawn_sh(&["-c", "exit 3"]);
        let pid = child.id() as i32;
        set_group(pid);
        match wait_foreground(pid) {
            FgOutcome::Exited(code) => assert_eq!(code, 3),
            other => panic!("expected exit 3, got {:?}", other),
        }
    }

    #[test]
    fn test_wait_foreground_stopped_then_resumed() {
        // A child that stops itself with SIGSTOP must be reported as stopped
        // (not reaped), then resume with SIGCONT and finish normally.
        let child = spawn_sh(&["-c", "kill -STOP $$; exit 7"]);
        let pid = child.id() as i32;
        set_group(pid);

        match wait_foreground(pid) {
            FgOutcome::Stopped => {}
            other => panic!("expected Stopped, got {:?}", other),
        }

        continue_job(pid);
        match wait_foreground(pid) {
            FgOutcome::Exited(code) => assert_eq!(code, 7),
            other => panic!("expected exit 7, got {:?}", other),
        }
    }
}
