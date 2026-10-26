#[cfg(feature = "use-dev-tty")]
pub(crate) mod tty;

#[cfg(not(feature = "use-dev-tty"))]
pub(crate) mod mio;

#[cfg(feature = "use-dev-tty")]
pub(crate) use self::tty::UnixInternalEventSource;

#[cfg(not(feature = "use-dev-tty"))]
pub(crate) use self::mio::UnixInternalEventSource;

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        os::unix::net::UnixStream,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use super::UnixInternalEventSource;
    use crate::{
        event::{source::EventSource, Event, InternalEvent, KeyCode},
        terminal::sys::file_descriptor::FileDesc,
    };

    fn descriptor(fd: impl Into<std::os::fd::OwnedFd>) -> FileDesc<'static> {
        let fd = fd.into();
        #[cfg(feature = "libc")]
        {
            use std::os::fd::IntoRawFd;
            FileDesc::new(fd.into_raw_fd(), true)
        }
        #[cfg(not(feature = "libc"))]
        FileDesc::Owned(fd)
    }

    // Run the read in a bounded child: the original bug never returns even
    // with a poll timeout, so a test thread alone cannot safely reproduce it.
    fn run_in_child(name: &str) -> bool {
        const CHILD: &str = "CROSSTERM_TERMINAL_LOSS_TEST";
        if std::env::var(CHILD).as_deref() == Ok(name) {
            return false;
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env(CHILD, name)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("terminal input reader did not return after terminal loss");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        true
    }

    #[test]
    fn closed_input_returns_eof() {
        if run_in_child(
            concat!(module_path!(), "::closed_input_returns_eof").trim_start_matches("crossterm::"),
        ) {
            return;
        }
        let (input, peer) = UnixStream::pair().unwrap();
        input.set_nonblocking(true).unwrap();
        let mut source = UnixInternalEventSource::from_file_descriptor(descriptor(input)).unwrap();
        drop(peer);
        let error = source
            .try_read(Some(Duration::from_millis(50)))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn terminal_read_error_propagates() {
        if run_in_child(
            concat!(module_path!(), "::terminal_read_error_propagates")
                .trim_start_matches("crossterm::"),
        ) {
            return;
        }
        use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
        use std::{ffi::OsStr, fs::File, os::unix::ffi::OsStrExt};
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::CLOEXEC).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let slave = File::options()
            .read(true)
            .write(true)
            .open(OsStr::from_bytes(name.to_bytes()))
            .unwrap();
        let mut source = UnixInternalEventSource::from_file_descriptor(descriptor(master)).unwrap();
        drop(slave);
        let error = source
            .try_read(Some(Duration::from_millis(50)))
            .unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(rustix::io::Errno::IO.raw_os_error())
        );
    }

    #[test]
    fn input_still_times_out_and_delivers_keys() {
        let (input, mut peer) = UnixStream::pair().unwrap();
        input.set_nonblocking(true).unwrap();
        let mut source = UnixInternalEventSource::from_file_descriptor(descriptor(input)).unwrap();
        assert!(source
            .try_read(Some(Duration::from_millis(10)))
            .unwrap()
            .is_none());
        peer.write_all(b"x").unwrap();
        assert!(matches!(
            source.try_read(Some(Duration::from_millis(50))).unwrap(),
            Some(InternalEvent::Event(Event::Key(key))) if key.code == KeyCode::Char('x')
        ));
    }
}
