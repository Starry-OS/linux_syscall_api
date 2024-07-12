extern crate alloc;
use alloc::{sync::Arc, vec::Vec};
use core::{
    mem::size_of,
    net::Ipv4Addr,
    ptr::copy_nonoverlapping,
    sync::atomic::{AtomicBool, AtomicU64},
};

use alloc::string::String;
use axerrno::{AxError, AxResult};
use axfs::api::{FileIO, FileIOType, OpenFlags, Read, Write};

use axlog::warn;
use axnet::{
    add_membership, from_core_sockaddr, into_core_sockaddr, poll_interfaces, IpAddr, SocketAddr,
    TcpSocket, UdpSocket,
};
use axsync::Mutex;
use num_enum::TryFromPrimitive;

use crate::{
    syscall_fs::ctype::pipe::{make_pipe, Pipe},
    LibcSocketAddr, SyscallError, SyscallResult, TimeVal,
};

pub const SOCKET_TYPE_MASK: usize = 0xFF;

#[derive(TryFromPrimitive, Clone, PartialEq, Eq)]
#[repr(usize)]
#[allow(non_camel_case_types)]
pub enum Domain {
    AF_UNIX = 1,
    AF_INET = 2,
}

#[derive(TryFromPrimitive, PartialEq, Eq, Copy, Clone, Debug)]
#[repr(usize)]
#[allow(non_camel_case_types)]
pub enum SocketType {
    /// Provides sequenced, reliable, two-way, connection-based byte streams.
    /// An out-of-band data transmission mechanism may be supported.
    SOCK_STREAM = 1,
    /// Supports datagrams (connectionless, unreliable messages of a fixed maximum length).
    SOCK_DGRAM = 2,
    /// Provides raw network protocol access.
    SOCK_RAW = 3,
    /// Provides a reliable datagram layer that does not guarantee ordering.
    SOCK_RDM = 4,
    /// Provides a sequenced, reliable, two-way connection-based data
    /// transmission path for datagrams of fixed maximum length;
    /// a consumer is required to read an entire packet with each input system call.
    SOCK_SEQPACKET = 5,
    /// Datagram Congestion Control Protocol socket
    SOCK_DCCP = 6,
    /// Obsolete and should not be used in new programs.
    SOCK_PACKET = 10,
}

/// Set O_NONBLOCK flag on the open fd
pub const SOCK_NONBLOCK: usize = 0x800;
/// Set FD_CLOEXEC flag on the new fd
pub const SOCK_CLOEXEC: usize = 0x80000;

#[derive(TryFromPrimitive, Debug)]
#[repr(usize)]
#[allow(non_camel_case_types)]
pub enum SocketOptionLevel {
    IP = 0,
    Socket = 1,
    Tcp = 6,
}

#[derive(TryFromPrimitive, Debug)]
#[repr(usize)]
#[allow(non_camel_case_types)]
pub enum IpOption {
    IP_MULTICAST_IF = 32,
    IP_MULTICAST_TTL = 33,
    IP_MULTICAST_LOOP = 34,
    IP_ADD_MEMBERSHIP = 35,
}

#[derive(TryFromPrimitive, Debug)]
#[repr(usize)]
#[allow(non_camel_case_types)]
pub enum SocketOption {
    SO_REUSEADDR = 2,
    SO_ERROR = 4,
    SO_DONTROUTE = 5,
    SO_SNDBUF = 7,
    SO_RCVBUF = 8,
    SO_KEEPALIVE = 9,
    SO_RCVTIMEO = 20,
    SO_SNDTIMEO = 21,
}

#[derive(TryFromPrimitive, PartialEq)]
#[repr(usize)]
#[allow(non_camel_case_types)]
pub enum TcpSocketOption {
    TCP_NODELAY = 1, // disable nagle algorithm and flush
    TCP_MAXSEG = 2,
    TCP_INFO = 11,
    TCP_CONGESTION = 13,
}

impl IpOption {
    pub fn set(&self, socket: &Socket, opt: &[u8]) -> SyscallResult {
        match self {
            IpOption::IP_MULTICAST_IF => {
                // 我们只会使用LOOPBACK作为多播接口
                Ok(0)
            }
            IpOption::IP_MULTICAST_TTL => {
                let mut inner = socket.inner.lock();
                match &mut *inner {
                    SocketInner::Udp(s) => {
                        let ttl = u8::from_ne_bytes(<[u8; 1]>::try_from(&opt[0..1]).unwrap());
                        s.set_socket_ttl(ttl);
                        Ok(0)
                    }
                    _ => panic!("setsockopt IP_MULTICAST_TTL on a non-udp socket"),
                }
            }
            IpOption::IP_MULTICAST_LOOP => Ok(0),
            IpOption::IP_ADD_MEMBERSHIP => {
                let multicast_addr = IpAddr::v4(opt[0], opt[1], opt[2], opt[3]);
                let interface_addr = IpAddr::v4(opt[4], opt[5], opt[6], opt[7]);
                // TODO add membership error handling
                add_membership(multicast_addr, interface_addr);
                Ok(0)
            }
        }
    }
}

impl SocketOption {
    pub fn set(&self, socket: &Socket, opt: &[u8]) -> SyscallResult {
        match self {
            SocketOption::SO_REUSEADDR => {
                if opt.len() < 4 {
                    panic!("can't read a int from socket opt value");
                }
                let opt_value = i32::from_ne_bytes(<[u8; 4]>::try_from(&opt[0..4]).unwrap());
                socket.set_reuse_addr(opt_value != 0);
                Ok(0)
            }
            SocketOption::SO_DONTROUTE => {
                if opt.len() < 4 {
                    panic!("can't read a int from socket opt value");
                }

                let opt_value = i32::from_ne_bytes(<[u8; 4]>::try_from(&opt[0..4]).unwrap());

                socket.set_reuse_addr(opt_value != 0);
                // socket.reuse_addr = opt_value != 0;
                Ok(0)
            }
            SocketOption::SO_SNDBUF => {
                if opt.len() < 4 {
                    panic!("can't read a int from socket opt value");
                }

                let opt_value = i32::from_ne_bytes(<[u8; 4]>::try_from(&opt[0..4]).unwrap());

                socket.set_send_buf_size(opt_value as u64);
                // socket.send_buf_size = opt_value as usize;
                Ok(0)
            }
            SocketOption::SO_RCVBUF => {
                if opt.len() < 4 {
                    panic!("can't read a int from socket opt value");
                }

                let opt_value = i32::from_ne_bytes(<[u8; 4]>::try_from(&opt[0..4]).unwrap());

                socket.set_recv_buf_size(opt_value as u64);
                // socket.recv_buf_size = opt_value as usize;
                Ok(0)
            }
            SocketOption::SO_KEEPALIVE => {
                if opt.len() < 4 {
                    panic!("can't read a int from socket opt value");
                }

                let opt_value = i32::from_ne_bytes(<[u8; 4]>::try_from(&opt[0..4]).unwrap());

                let interval = if opt_value != 0 {
                    Some(axnet::Duration::from_secs(45))
                } else {
                    None
                };

                let mut inner = socket.inner.lock();

                match &mut (*inner) {
                    SocketInner::Udp(_) => {
                        warn!("[setsockopt()] set SO_KEEPALIVE on udp socket, ignored")
                    }
                    SocketInner::Tcp(s) => s.with_socket_mut(|s| match s {
                        Some(s) => s.set_keep_alive(interval),
                        None => warn!(
                            "[setsockopt()] set keep-alive for tcp socket not created, ignored"
                        ),
                    }),
                };
                drop(inner);
                socket.set_recv_buf_size(opt_value as u64);
                // socket.recv_buf_size = opt_value as usize;
                Ok(0)
            }
            SocketOption::SO_RCVTIMEO => {
                if opt.len() < size_of::<TimeVal>() {
                    panic!("can't read a timeval from socket opt value");
                }

                let timeout = unsafe { *(opt.as_ptr() as *const TimeVal) };
                socket.set_recv_timeout(if timeout.sec == 0 && timeout.usec == 0 {
                    None
                } else {
                    Some(timeout)
                });
                Ok(0)
            }
            SocketOption::SO_ERROR => {
                panic!("can't set SO_ERROR");
            }
            SocketOption::SO_SNDTIMEO => Err(SyscallError::EPERM),
        }
    }

    pub fn get(&self, socket: &Socket, opt_value: *mut u8, opt_len: *mut u32) {
        let buf_len = unsafe { *opt_len } as usize;

        match self {
            SocketOption::SO_REUSEADDR => {
                let value: i32 = if socket.get_reuse_addr() { 1 } else { 0 };

                if buf_len < 4 {
                    panic!("can't write a int to socket opt value");
                }

                unsafe {
                    copy_nonoverlapping(&value.to_ne_bytes() as *const u8, opt_value, 4);
                    *opt_len = 4;
                }
            }
            SocketOption::SO_DONTROUTE => {
                if buf_len < 4 {
                    panic!("can't write a int to socket opt value");
                }

                let size: i32 = if socket.dont_route { 1 } else { 0 };

                unsafe {
                    copy_nonoverlapping(&size.to_ne_bytes() as *const u8, opt_value, 4);
                    *opt_len = 4;
                }
            }
            SocketOption::SO_SNDBUF => {
                if buf_len < 4 {
                    panic!("can't write a int to socket opt value");
                }

                let size: i32 = socket.get_send_buf_size() as i32;

                unsafe {
                    copy_nonoverlapping(&size.to_ne_bytes() as *const u8, opt_value, 4);
                    *opt_len = 4;
                }
            }
            SocketOption::SO_RCVBUF => {
                if buf_len < 4 {
                    panic!("can't write a int to socket opt value");
                }

                let size: i32 = socket.get_recv_buf_size() as i32;

                unsafe {
                    copy_nonoverlapping(&size.to_ne_bytes() as *const u8, opt_value, 4);
                    *opt_len = 4;
                }
            }
            SocketOption::SO_KEEPALIVE => {
                if buf_len < 4 {
                    panic!("can't write a int to socket opt value");
                }

                let mut inner = socket.inner.lock();
                let keep_alive: i32 = match &mut *inner {
                    SocketInner::Udp(_) => {
                        warn!("[getsockopt()] get SO_KEEPALIVE on udp socket, returning false");
                        0
                    }
                    SocketInner::Tcp(s) => s.with_socket(|s| match s {
                        Some(s) => if s.keep_alive().is_some() { 1 } else { 0 },
                        None => {warn!(
                            "[setsockopt()] set keep-alive for tcp socket not created, returning false"
                        );
                            0},
                    }),
                };
                drop(inner);

                unsafe {
                    copy_nonoverlapping(&keep_alive.to_ne_bytes() as *const u8, opt_value, 4);
                    *opt_len = 4;
                }
            }
            SocketOption::SO_RCVTIMEO => {
                if buf_len < size_of::<TimeVal>() {
                    panic!("can't write a timeval to socket opt value");
                }

                unsafe {
                    match socket.get_recv_timeout() {
                        Some(time) => copy_nonoverlapping(
                            (&time) as *const TimeVal,
                            opt_value as *mut TimeVal,
                            1,
                        ),
                        None => {
                            copy_nonoverlapping(&0u8 as *const u8, opt_value, size_of::<TimeVal>())
                        }
                    }

                    *opt_len = size_of::<TimeVal>() as u32;
                }
            }
            SocketOption::SO_ERROR => {
                // 当前没有存储错误列表，因此不做处理
            }
            SocketOption::SO_SNDTIMEO => {
                panic!("unimplemented!")
            }
        }
    }
}

impl TcpSocketOption {
    pub fn set(&self, raw_socket: &Socket, opt: &[u8]) -> SyscallResult {
        let mut inner = raw_socket.inner.lock();
        let socket = match &mut *inner {
            SocketInner::Tcp(ref mut s) => s,
            _ => panic!("calling tcp option on a wrong type of socket"),
        };

        match self {
            TcpSocketOption::TCP_NODELAY => {
                if opt.len() < 4 {
                    panic!("can't read a int from socket opt value");
                }
                let opt_value = i32::from_ne_bytes(<[u8; 4]>::try_from(&opt[0..4]).unwrap());

                let _ = socket.set_nagle_enabled(opt_value == 0);
                let _ = socket.flush();
                Ok(0)
            }
            TcpSocketOption::TCP_INFO => {
                // TODO: support the protocal
                Ok(0)
            }
            TcpSocketOption::TCP_CONGESTION => {
                raw_socket.set_congestion(String::from_utf8(Vec::from(opt)).unwrap());
                Ok(0)
            }
            _ => {
                unimplemented!()
            }
        }
    }

    pub fn get(&self, raw_socket: &Socket, opt_value: *mut u8, opt_len: *mut u32) {
        let inner = raw_socket.inner.lock();
        let socket = match &*inner {
            SocketInner::Tcp(ref s) => s,
            _ => panic!("calling tcp option on a wrong type of socket"),
        };

        let buf_len = unsafe { *opt_len };

        match self {
            TcpSocketOption::TCP_NODELAY => {
                if buf_len < 4 {
                    panic!("can't write a int to socket opt value");
                }

                let value: i32 = if socket.nagle_enabled() { 0 } else { 1 };

                let value = value.to_ne_bytes();

                unsafe {
                    copy_nonoverlapping(&value as *const u8, opt_value, 4);
                    *opt_len = 4;
                }
            }
            TcpSocketOption::TCP_MAXSEG => {
                let len = size_of::<usize>();

                let value: usize = 1500;

                unsafe {
                    copy_nonoverlapping(&value as *const usize as *const u8, opt_value, len);
                    *opt_len = len as u32;
                };
            }
            TcpSocketOption::TCP_INFO => {}
            TcpSocketOption::TCP_CONGESTION => {
                let bytes = raw_socket.get_congestion();
                let bytes = bytes.as_bytes();

                unsafe {
                    copy_nonoverlapping(bytes.as_ptr(), opt_value, bytes.len());
                    *opt_len = bytes.len() as u32;
                };
            }
        }
    }
}

/// 包装内部的不同协议 Socket
/// 类似 FileDesc，impl FileIO 后加入fd_list
#[allow(dead_code)]
pub struct Socket {
    domain: Domain,
    socket_type: SocketType,

    /// Type of the socket protocol used
    pub inner: Mutex<SocketInner>,
    /// Whether the socket is set to close on exec
    pub close_exec: bool,
    recv_timeout: Mutex<Option<TimeVal>>,

    /// FIXME: temp for socket pair
    pub buffer: Option<Arc<Pipe>>,

    // fake options
    dont_route: bool,
    send_buf_size: AtomicU64,
    recv_buf_size: AtomicU64,
    congestion: Mutex<String>,
}

/// The transport protocol used by the socket
pub enum SocketInner {
    /// TCP socket
    Tcp(TcpSocket),
    /// UDP socket
    Udp(UdpSocket),
}

impl Socket {
    fn get_recv_timeout(&self) -> Option<TimeVal> {
        *self.recv_timeout.lock()
    }
    fn get_reuse_addr(&self) -> bool {
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => unimplemented!("get_reuse_addr on other socket"),
            SocketInner::Udp(s) => s.is_reuse_addr(),
        }
    }

    fn get_send_buf_size(&self) -> u64 {
        self.send_buf_size
            .load(core::sync::atomic::Ordering::Acquire)
    }

    fn get_recv_buf_size(&self) -> u64 {
        self.recv_buf_size
            .load(core::sync::atomic::Ordering::Acquire)
    }

    fn get_congestion(&self) -> String {
        self.congestion.lock().clone()
    }

    fn set_recv_timeout(&self, val: Option<TimeVal>) {
        *self.recv_timeout.lock() = val;
    }

    fn set_reuse_addr(&self, flag: bool) {
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => (),
            SocketInner::Udp(s) => s.set_reuse_addr(flag),
        }
    }

    fn set_send_buf_size(&self, size: u64) {
        self.send_buf_size
            .store(size, core::sync::atomic::Ordering::Release)
    }

    fn set_recv_buf_size(&self, size: u64) {
        self.recv_buf_size
            .store(size, core::sync::atomic::Ordering::Release)
    }

    fn set_congestion(&self, congestion: String) {
        *self.congestion.lock() = congestion;
    }

    /// Create a new socket with the given domain and socket type.
    pub fn new(domain: Domain, socket_type: SocketType) -> Self {
        let inner = match socket_type {
            SocketType::SOCK_STREAM | SocketType::SOCK_SEQPACKET => {
                SocketInner::Tcp(TcpSocket::new())
            }
            SocketType::SOCK_DGRAM => SocketInner::Udp(UdpSocket::new()),
            _ => unimplemented!(),
        };
        Self {
            domain,
            socket_type,
            inner: Mutex::new(inner),
            close_exec: false,
            buffer: None,
            recv_timeout: Mutex::new(None),
            dont_route: false,
            send_buf_size: AtomicU64::new(64 * 1024),
            recv_buf_size: AtomicU64::new(64 * 1024),
            congestion: Mutex::new(String::from("reno")),
        }
    }

    /// set the socket to non-blocking mode
    pub fn set_nonblocking(&self, nonblocking: bool) {
        let inner = self.inner.lock();

        match &*inner {
            SocketInner::Tcp(s) => s.set_nonblocking(nonblocking),
            SocketInner::Udp(s) => s.set_nonblocking(nonblocking),
        }
    }

    /// Return the non-blocking flag of the socket
    pub fn is_nonblocking(&self) -> bool {
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => s.is_nonblocking(),
            SocketInner::Udp(s) => s.is_nonblocking(),
        }
    }

    /// Socket may send or recv.
    pub fn is_connected(&self) -> bool {
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => s.is_connected(),
            SocketInner::Udp(s) => s.with_socket(|s| s.is_open()),
        }
    }

    /// Return bound address.
    pub fn name(&self) -> AxResult<SocketAddr> {
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => s.local_addr(),
            SocketInner::Udp(s) => s.local_addr(),
        }
        .map(from_core_sockaddr)
    }

    /// Return peer address.
    pub fn peer_name(&self) -> AxResult<SocketAddr> {
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => s.peer_addr(),
            SocketInner::Udp(s) => s.peer_addr(),
        }
        .map(from_core_sockaddr)
    }

    /// Bind the socket to the given address.
    pub fn bind(&self, addr: SocketAddr) -> AxResult {
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => s.bind(into_core_sockaddr(addr)),
            SocketInner::Udp(s) => s.bind(into_core_sockaddr(addr)),
        }
    }

    /// Listen to the bound address.
    ///
    /// Only support socket with type SOCK_STREAM or SOCK_SEQPACKET
    ///
    /// Err(Unsupported): EOPNOTSUPP
    pub fn listen(&self) -> AxResult {
        if self.socket_type != SocketType::SOCK_STREAM
            && self.socket_type != SocketType::SOCK_SEQPACKET
        {
            return Err(AxError::Unsupported);
        }
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => s.listen(),
            SocketInner::Udp(_) => Err(AxError::Unsupported),
        }
    }

    /// Accept a new connection.
    pub fn accept(&self) -> AxResult<(Self, SocketAddr)> {
        if self.socket_type != SocketType::SOCK_STREAM
            && self.socket_type != SocketType::SOCK_SEQPACKET
        {
            return Err(AxError::Unsupported);
        }
        let inner = self.inner.lock();
        let new_socket = match &*inner {
            SocketInner::Tcp(s) => s.accept()?,
            SocketInner::Udp(_) => Err(AxError::Unsupported)?,
        };
        let addr = new_socket.peer_addr()?;

        Ok((
            Self {
                domain: self.domain.clone(),
                socket_type: self.socket_type.clone(),
                inner: Mutex::new(SocketInner::Tcp(new_socket)),
                close_exec: false,
                buffer: None,
                recv_timeout: Mutex::new(None),
                dont_route: false,
                send_buf_size: AtomicU64::new(64 * 1024),
                recv_buf_size: AtomicU64::new(64 * 1024),
                congestion: Mutex::new(String::from("reno")),
            },
            from_core_sockaddr(addr),
        ))
    }

    /// Connect to the given address.
    pub fn connect(&self, addr: SocketAddr) -> AxResult {
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => s.connect(into_core_sockaddr(addr)),
            SocketInner::Udp(s) => s.connect(into_core_sockaddr(addr)),
        }
    }

    #[allow(unused)]
    /// whether the socket is bound with a local address
    pub fn is_bound(&self) -> bool {
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => s.local_addr().is_ok(),
            SocketInner::Udp(s) => s.local_addr().is_ok(),
        }
    }
    #[allow(unused)]
    /// let the socket send data to the given address
    pub fn sendto(&self, buf: &[u8], addr: SocketAddr) -> AxResult<usize> {
        if self.domain == Domain::AF_UNIX {
            return self.buffer.as_ref().unwrap().write(buf);
        }
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => s.send(buf),
            SocketInner::Udp(s) => s.send_to(buf, into_core_sockaddr(addr)),
        }
    }

    /// let the socket receive data and write it to the given buffer
    pub fn recv_from(&self, buf: &mut [u8]) -> AxResult<(usize, SocketAddr)> {
        if self.domain == Domain::AF_UNIX {
            let ans = self.buffer.as_ref().unwrap().read(buf)?;
            return Ok((
                ans,
                SocketAddr::new(IpAddr::Ipv4(axnet::Ipv4Addr::new(127, 0, 0, 1)), 0),
            ));
        }
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => {
                let addr = s.peer_addr()?;

                match self.get_recv_timeout() {
                    Some(time) => s.recv_timeout(buf, time.turn_to_ticks()),
                    None => s.recv(buf),
                }
                .map(|len| (len, from_core_sockaddr(addr)))
            }
            SocketInner::Udp(s) => match self.get_recv_timeout() {
                Some(time) => s
                    .recv_from_timeout(buf, time.turn_to_ticks())
                    .map(|(val, addr)| (val, from_core_sockaddr(addr))),
                None => s
                    .recv_from(buf)
                    .map(|(val, addr)| (val, from_core_sockaddr(addr))),
            },
        }
    }

    /// For shutdown(fd, SHUT_WR)
    pub fn shutdown(&self) {
        let mut inner = self.inner.lock();
        match &mut *inner {
            SocketInner::Udp(s) => {
                s.shutdown();
            }
            SocketInner::Tcp(s) => {
                s.close();
            }
        };
    }

    /// For shutdown(fd, SHUT_RDWR)
    pub fn abort(&self) {
        let mut inner = self.inner.lock();
        match &mut *inner {
            SocketInner::Udp(s) => {
                let _ = s.shutdown();
            }
            SocketInner::Tcp(s) => s.with_socket_mut(|s| {
                if let Some(s) = s {
                    s.abort();
                }
            }),
        }
    }
}

impl FileIO for Socket {
    fn read(&self, buf: &mut [u8]) -> AxResult<usize> {
        if self.domain == Domain::AF_UNIX {
            return self.buffer.as_ref().unwrap().read(buf);
        }
        let mut inner = self.inner.lock();
        match &mut *inner {
            SocketInner::Tcp(s) => s.read(buf),
            SocketInner::Udp(s) => s.read(buf),
        }
    }

    fn write(&self, buf: &[u8]) -> AxResult<usize> {
        if self.domain == Domain::AF_UNIX {
            return self.buffer.as_ref().unwrap().write(buf);
        }
        let mut inner = self.inner.lock();
        match &mut *inner {
            SocketInner::Tcp(s) => s.write(buf),
            SocketInner::Udp(s) => s.write(buf),
        }
    }

    fn flush(&self) -> AxResult {
        Err(AxError::Unsupported)
    }

    fn readable(&self) -> bool {
        if self.domain == Domain::AF_UNIX {
            return self.buffer.as_ref().unwrap().readable();
        }
        poll_interfaces();
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => s.poll().map_or(false, |p| p.readable),
            SocketInner::Udp(s) => s.poll().map_or(false, |p| p.readable),
        }
    }

    fn writable(&self) -> bool {
        if self.domain == Domain::AF_UNIX {
            return self.buffer.as_ref().unwrap().writable();
        }
        poll_interfaces();
        let inner = self.inner.lock();
        match &*inner {
            SocketInner::Tcp(s) => s.poll().map_or(false, |p| p.writable),
            SocketInner::Udp(s) => s.poll().map_or(false, |p| p.writable),
        }
    }

    fn executable(&self) -> bool {
        false
    }

    fn get_type(&self) -> FileIOType {
        FileIOType::Socket
    }

    fn get_status(&self) -> OpenFlags {
        let mut flags = OpenFlags::default();

        if self.close_exec {
            flags |= OpenFlags::CLOEXEC;
        }

        if self.is_nonblocking() {
            flags |= OpenFlags::NON_BLOCK;
        }

        flags
    }

    fn set_status(&self, flags: OpenFlags) -> bool {
        self.set_nonblocking(flags.contains(OpenFlags::NON_BLOCK));

        true
    }

    fn ready_to_read(&self) -> bool {
        self.readable()
    }

    fn ready_to_write(&self) -> bool {
        self.writable()
    }
}

/// return sockerpair read write
pub fn make_socketpair(socket_type: usize) -> (Arc<Socket>, Arc<Socket>) {
    let s_type = SocketType::try_from(socket_type & SOCKET_TYPE_MASK).unwrap();
    let mut fd1 = Socket::new(Domain::AF_UNIX, s_type);
    let mut fd2 = Socket::new(Domain::AF_UNIX, s_type);
    let mut pipe_flag = OpenFlags::empty();
    if socket_type & SOCK_NONBLOCK != 0 {
        pipe_flag |= OpenFlags::NON_BLOCK;
        fd1.set_nonblocking(true);
        fd2.set_nonblocking(true);
    }
    if socket_type & SOCK_CLOEXEC != 0 {
        pipe_flag |= OpenFlags::CLOEXEC;
        fd1.close_exec = true;
        fd2.close_exec = true;
        axlog::info!("set close on exec");
    }
    let (pipe1, pipe2) = make_pipe(pipe_flag);
    fd1.buffer = Some(pipe1);
    fd2.buffer = Some(pipe2);
    (Arc::new(fd1), Arc::new(fd2))
}

/// Turn a socket address buffer into a SocketAddr
///
/// Only support INET (ipv4)
pub unsafe fn socket_address_from(addr: *const u8, socket: &Socket) -> SocketAddr {
    let addr = addr as *const u16;
    match socket.domain {
        Domain::AF_UNIX => unimplemented!(),
        Domain::AF_INET => {
            let port = u16::from_be(*addr.add(1));
            let a = (*(addr.add(2) as *const u32)).to_le_bytes();

            let addr = IpAddr::v4(a[0], a[1], a[2], a[3]);
            SocketAddr { addr, port }
        }
    }
}
/// Only support INET (ipv4)
///
/// ipv4 socket address buffer:
/// socket_domain (address_family) u16
/// port u16 (big endian)
/// addr u32 (big endian)
///
/// TODO: Returns error if buf or buf_len is in invalid memory
pub unsafe fn socket_address_to(
    addr: SocketAddr,
    buf: *mut u8,
    mut buf_len: usize,
    buf_len_addr: *mut u32,
) -> AxResult {
    *buf_len_addr = core::mem::size_of::<LibcSocketAddr>() as u32;
    // 写入 AF_INET
    if buf_len == 0 {
        return Ok(());
    }
    let domain = (Domain::AF_INET as u16).to_ne_bytes();
    let write_len = buf_len.min(2);
    copy_nonoverlapping(domain.as_ptr(), buf, write_len);
    let buf = buf.add(write_len);
    buf_len -= write_len;

    // 写入 port
    if buf_len == 0 {
        return Ok(());
    }
    let port = &addr.port.to_be_bytes();
    let write_len = buf_len.min(2);
    copy_nonoverlapping(port.as_ptr(), buf, write_len);
    let buf = buf.add(write_len);
    buf_len -= write_len;

    // 写入 address, only support ipv4
    axlog::warn!("Only support ipv4");
    if buf_len == 0 {
        return Ok(());
    }
    let address = &addr.addr.as_bytes();
    let write_len = buf_len.min(4);
    copy_nonoverlapping(address.as_ptr(), buf, write_len);

    Ok(())
}
