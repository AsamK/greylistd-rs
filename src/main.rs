use crossbeam_channel::unbounded;
use greylistd::config::Config;
use greylistd::App;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;

fn main() -> Result<(), anyhow::Error> {
    let file_config = "/etc/greylistd/config";

    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    let (stop_sender, stop_receiver) = unbounded();
    std::thread::spawn(move || {
        if signals.forever().next().is_some() {
            stop_sender.send(()).unwrap();
        }
    });

    loop {
        let config = Config::load(file_config)?;

        let socket_path;
        let listener = if let Some(listener) = get_systemd_unix_listener()? {
            socket_path = None;
            listener
        } else {
            socket_path = Some(config.socket.path.clone());
            let mode = u32::from_str_radix(&config.socket.mode, 8)?;
            let listener = UnixListener::bind(&config.socket.path)?;
            fs::set_permissions(&config.socket.path, fs::Permissions::from_mode(mode))?;
            listener
        };

        let app = App::new(config)?;

        let reload = app.run(listener, stop_receiver.clone())?;

        if let Some(socket_path) = socket_path {
            fs::remove_file(&socket_path)?;
        }
        if !reload {
            break;
        }
    }

    Ok(())
}

fn get_systemd_unix_listener() -> Result<Option<UnixListener>, anyhow::Error> {
    #[cfg(feature = "systemd")]
    {
        use std::os::fd::FromRawFd;
        use systemd::daemon::{Listening, SocketType};

        let fds = systemd::daemon::listen_fds(true)?;

        for fd in fds.iter() {
            if systemd::daemon::is_socket_unix::<String>(
                fd,
                Some(SocketType::Stream),
                Listening::IsListening,
                None,
            )? {
                return Ok(Some(unsafe { UnixListener::from_raw_fd(fd) }));
            }
        }
    }

    Ok(None)
}
