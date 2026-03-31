use std::io::{self, Read, Write};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use crate::Error;

#[cfg(target_arch = "wasm32")]
const AF_INET: i32 = 2;
#[cfg(target_arch = "wasm32")]
const SOCK_STREAM: i32 = 1;
#[cfg(target_arch = "wasm32")]
const SHUT_RDWR: i32 = 2;
#[cfg(target_arch = "wasm32")]
const WASI_SUCCESS: i32 = 0;

#[cfg(target_arch = "wasm32")]
#[repr(C)]
struct Ciovec {
    buf: *const u8,
    buf_len: usize,
}

#[cfg(target_arch = "wasm32")]
#[repr(C)]
struct Iovec {
    buf: *mut u8,
    buf_len: usize,
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "wasi_snapshot_preview1")]
unsafe extern "C" {
    fn sock_open(af: i32, socktype: i32, proto: i32, fd_out: *mut u32) -> i32;
    fn sock_connect(fd: i32, addr_ptr: *const u8, addr_len: i32) -> i32;
    fn sock_send(
        fd: i32,
        si_data_ptr: *const Ciovec,
        si_data_len: i32,
        si_flags: i32,
        datalen_out_ptr: *mut u32,
    ) -> i32;
    fn sock_recv(
        fd: i32,
        ri_data_ptr: *const Iovec,
        ri_data_len: i32,
        ri_flags: i32,
        datalen_out_ptr: *mut u32,
        roflags_out_ptr: *mut u16,
    ) -> i32;
    fn sock_shutdown(fd: i32, how: i32) -> i32;
}

pub(crate) enum RawSocket {
    #[cfg(target_arch = "wasm32")]
    Wasi { fd: i32 },
    #[cfg(not(target_arch = "wasm32"))]
    Host(std::net::TcpStream),
}

impl RawSocket {
    pub(crate) fn connect(ip: Ipv4Addr, port: u16) -> Result<Self, Error> {
        Self::connect_timeout(ip, port, Duration::from_secs(15))
    }

    pub(crate) fn connect_timeout(
        ip: Ipv4Addr,
        port: u16,
        timeout: Duration,
    ) -> Result<Self, Error> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = timeout;
            let mut fd = 0u32;
            let open_errno = unsafe { sock_open(AF_INET, SOCK_STREAM, 0, &mut fd as *mut u32) };
            if open_errno != WASI_SUCCESS {
                return Err(Self::errno("sock_open", open_errno).into());
            }

            let addr = [
                0u8,
                0,
                (port >> 8) as u8,
                (port & 0xff) as u8,
                ip.octets()[0],
                ip.octets()[1],
                ip.octets()[2],
                ip.octets()[3],
            ];
            let connect_errno =
                unsafe { sock_connect(fd as i32, addr.as_ptr(), addr.len() as i32) };
            if connect_errno != WASI_SUCCESS {
                let _ = unsafe { sock_shutdown(fd as i32, SHUT_RDWR) };
                return Err(Self::errno("sock_connect", connect_errno).into());
            }

            Ok(Self::Wasi { fd: fd as i32 })
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::net::{SocketAddr, SocketAddrV4, TcpStream};

            let addr = SocketAddr::V4(SocketAddrV4::new(ip, port));
            let stream = TcpStream::connect_timeout(&addr, timeout)?;
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))?;
            Ok(Self::Host(stream))
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn errno(op: &str, errno: i32) -> io::Error {
        io::Error::other(format!("{op} failed with errno {errno}"))
    }
}

pub(crate) fn connect_any(
    addrs: &[Ipv4Addr],
    port: u16,
    timeout: Duration,
) -> Result<Duration, Error> {
    let mut last_error = None;
    for ip in addrs {
        let started_at = Instant::now();
        match RawSocket::connect_timeout(*ip, port, timeout) {
            Ok(_socket) => return Ok(started_at.elapsed()),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        Error::Dns(format!(
            "no IPv4 addresses available for TCP connect on port {port}"
        ))
    }))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::net::{Ipv4Addr, TcpListener};
    use std::thread;
    use std::time::Duration;

    use super::RawSocket;

    #[test]
    fn connect_timeout_reaches_local_listener() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind test listener");
        let port = listener.local_addr().expect("local addr").port();
        let _accept_thread = thread::spawn(move || {
            let _ = listener.accept();
        });

        RawSocket::connect_timeout(Ipv4Addr::LOCALHOST, port, Duration::from_millis(500))
            .expect("connect to local listener");
    }
}

impl Read for RawSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(target_arch = "wasm32")]
        {
            let Self::Wasi { fd } = self;
            if buf.is_empty() {
                return Ok(0);
            }

            let iov = Iovec {
                buf: buf.as_mut_ptr(),
                buf_len: buf.len(),
            };
            let mut read = 0u32;
            let mut roflags = 0u16;
            let errno = unsafe {
                sock_recv(
                    *fd,
                    &iov as *const Iovec,
                    1,
                    0,
                    &mut read as *mut u32,
                    &mut roflags as *mut u16,
                )
            };
            if errno != WASI_SUCCESS {
                return Err(Self::errno("sock_recv", errno));
            }

            Ok(read as usize)
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let Self::Host(stream) = self;
            stream.read(buf)
        }
    }
}

impl Write for RawSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(target_arch = "wasm32")]
        {
            let Self::Wasi { fd } = self;
            if buf.is_empty() {
                return Ok(0);
            }

            let iov = Ciovec {
                buf: buf.as_ptr(),
                buf_len: buf.len(),
            };
            let mut written = 0u32;
            let errno =
                unsafe { sock_send(*fd, &iov as *const Ciovec, 1, 0, &mut written as *mut u32) };
            if errno != WASI_SUCCESS {
                return Err(Self::errno("sock_send", errno));
            }

            Ok(written as usize)
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let Self::Host(stream) = self;
            stream.write(buf)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        #[cfg(target_arch = "wasm32")]
        {
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let Self::Host(stream) = self;
            stream.flush()
        }
    }
}

impl Drop for RawSocket {
    fn drop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            let Self::Wasi { fd } = self;
            unsafe {
                let _ = sock_shutdown(*fd, SHUT_RDWR);
            }
        }
    }
}
