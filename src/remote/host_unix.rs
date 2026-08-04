//! Unix remote-host side of the SSH stdio bridge.

use std::io;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

pub(crate) fn run_remote_client_bridge() -> io::Result<()> {
    reject_independent_client_socket_override()?;
    ensure_remote_server_running()?;

    let socket_path = crate::server::socket_paths::client_socket_path();
    let stream = UnixStream::connect(&socket_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to connect to remote Herdr client socket {}: {err}",
                socket_path.display()
            ),
        )
    })?;

    let mut stdout = io::stdout().lock();
    let mut socket_to_stdout = stream.try_clone()?;
    let mut stdin_to_socket = stream;

    let _upload = thread::spawn(move || {
        let mut stdin = io::stdin();
        let _ = copy_flush(&mut stdin, &mut stdin_to_socket);
        let _ = stdin_to_socket.shutdown(std::net::Shutdown::Write);
    });

    copy_flush(&mut socket_to_stdout, &mut stdout).map(|_| ())
}

fn copy_flush<R: io::Read, W: io::Write>(reader: &mut R, writer: &mut W) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        writer.write_all(&buffer[..read])?;
        writer.flush()?;
        total += read as u64;
    }
}

fn reject_independent_client_socket_override() -> io::Result<()> {
    let independent = client_socket_override_is_independent(
        crate::session::explicit_session_requested(),
        std::env::var_os(crate::api::SOCKET_PATH_ENV_VAR).is_some(),
        std::env::var_os(crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR).is_some(),
    );
    if independent {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HERDR_CLIENT_SOCKET_PATH cannot be used with remote-client-bridge; set HERDR_SOCKET_PATH so the API and client sockets identify the same server",
        ));
    }
    Ok(())
}

fn client_socket_override_is_independent(
    explicit_session: bool,
    api_override: bool,
    client_override: bool,
) -> bool {
    !explicit_session && !api_override && client_override
}

fn ensure_remote_server_running() -> io::Result<()> {
    let socket_path = crate::server::socket_paths::client_socket_path();
    if crate::server::autodetect::is_server_listening() {
        let status = crate::api::read_runtime_status_at(
            &crate::api::socket_path(),
            Duration::from_millis(500),
        )?
        .ok_or_else(|| io::Error::other("remote server status API is unavailable"))?;
        if status.protocol == Some(crate::protocol::PROTOCOL_VERSION) {
            return Ok(());
        }
        return Err(io::Error::other(
            "remote herdr server must restart before this bridge can attach; rerun `herdr --remote` from an interactive terminal to approve stopping it",
        ));
    }

    crate::server::autodetect::spawn_server_daemon()?;
    crate::server::autodetect::wait_for_server_socket(&socket_path, Duration::from_secs(5))
}

#[cfg(test)]
mod tests {
    use super::client_socket_override_is_independent;

    #[test]
    fn client_only_socket_override_is_rejected_without_session_or_api_identity() {
        assert!(client_socket_override_is_independent(false, false, true));
        assert!(!client_socket_override_is_independent(true, false, true));
        assert!(!client_socket_override_is_independent(false, true, true));
        assert!(!client_socket_override_is_independent(false, false, false));
    }
}
