use std::io;
use tokio::net::UdpSocket;

const DAEMON_HEADER: &[u8] = b"{\"format\": \"json\", \"version\": 1}\n";
const DEFAULT_UDP_REMOTE_PORT: u16 = 2000;

pub(crate) struct DaemonClient<S: ClientState> {
    state: S,
}

pub struct Start {
    remote_port: u16,
}

pub struct Connected {
    sock: UdpSocket,
}

pub(crate) trait ClientState {}
impl ClientState for Start {}
impl ClientState for Connected {}

impl DaemonClient<Start> {
    pub(crate) fn new(remote_port: u16) -> Self {
        DaemonClient {
            state: Start { remote_port },
        }
    }

    pub(crate) async fn connect(&self) -> io::Result<DaemonClient<Connected>> {
        // Let the OS choose an IP and port for us...
        let sock = UdpSocket::bind("0.0.0.0:0").await?;
        let remote_addr = format!("127.0.0.1:{}", self.state.remote_port);
        sock.connect(remote_addr).await?;
        Ok(DaemonClient {
            state: Connected { sock },
        })
    }
}

impl Default for DaemonClient<Start> {
    fn default() -> Self {
        DaemonClient::new(DEFAULT_UDP_REMOTE_PORT)
    }
}

impl DaemonClient<Connected> {
    pub(crate) async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        let newline = b"\n";
        self.state
            .sock
            .send(&[DAEMON_HEADER, buf, newline].concat())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn send_data() {
        let buf = b"{\"hello\": \"world\"}";
        let client: DaemonClient<Start> = Default::default();
        let client = client.connect().await.unwrap();
        let len = client.send(buf).await.unwrap();
        assert_eq!(len, DAEMON_HEADER.len() + buf.len());
    }
}
