//! Helpers shared by the daemon integration tests.

use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::Mutex;

/// A local TCP port nobody listens on, for a daemon or aria2 about to start.
///
/// Binding to port 0 and dropping the listener can hand the same port to two
/// callers (or to a parallel test binary) before the daemon binds it. Ports
/// are therefore taken from a window derived from the process id, never
/// handed out twice by one process, and skipped while something listens.
pub fn free_port() -> u16 {
    static GIVEN: Mutex<Option<HashSet<u16>>> = Mutex::new(None);
    const WINDOW: u32 = 256;
    let base = 10_000 + (std::process::id() % 190) * WINDOW;
    let mut given = GIVEN.lock().unwrap_or_else(|e| e.into_inner());
    let given = given.get_or_insert_with(HashSet::new);
    for i in 0..WINDOW {
        let port = u16::try_from(base + i).unwrap();
        if !given.contains(&port) && TcpListener::bind(("127.0.0.1", port)).is_ok() {
            given.insert(port);
            return port;
        }
    }
    // Window exhausted: fall back to an ephemeral port.
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
