#![allow(unused)]
use axfs::api::{FileIO, FileIOType};
extern crate alloc;
use alloc::sync::{Arc, Weak};
use axerrno::AxResult;
use axlog::info;

use axsync::Mutex;
use axtask::yield_now;

/// IPC 
pub struct SocketPair {
    #[allow(unused)]
    readable: bool,
    #[allow(unused)]
    writable: bool,
    peer_buffer: Arc<Mutex<SocketRingBuffer>>,
    self_buffer: Arc<Mutex<SocketRingBuffer>>,
    non_block: bool,
}

const RING_BUFFER_SIZE: usize = 0x4000;

#[derive(Copy, Clone, PartialEq)]
enum RingBufferStatus {
    Full,
    Empty,
    Normal,
}

pub struct SocketRingBuffer {
    arr: [u8; RING_BUFFER_SIZE],
    head: usize,
    tail: usize,
    status: RingBufferStatus,
    // write_end should be weak, or memory leak
    write_end: Option<Weak<SocketPair>>,
}

impl SocketRingBuffer {
    /// create a empty UNIX socket ringbuffer
    pub fn new() -> Self {
        Self {
            arr: [0; RING_BUFFER_SIZE],
            head: 0,
            tail: 0,
            status: RingBufferStatus::Empty,
            write_end: None,
        }
    }

    /// set `Arc::downgrade` write_end
    pub fn set_write_end(&mut self, write_end: &Arc<SocketPair>) {
        self.write_end = Some(Arc::downgrade(write_end));
    }

    /// write one byte and set ringbuffer status
    pub fn write_byte(&mut self, byte: u8) {
        self.status = RingBufferStatus::Normal;
        self.arr[self.tail] = byte;
        self.tail = (self.tail + 1) % RING_BUFFER_SIZE;
        if self.tail == self.head {
            self.status = RingBufferStatus::Full;
        }
    }
    /// read one byte and set ringbuffer status
    pub fn read_byte(&mut self) -> u8 {
        self.status = RingBufferStatus::Normal;
        let c = self.arr[self.head];
        self.head = (self.head + 1) % RING_BUFFER_SIZE;
        if self.head == self.tail {
            self.status = RingBufferStatus::Empty;
        }
        c
    }
    pub fn available_read(&self) -> usize {
        if self.status == RingBufferStatus::Empty {
            0
        } else if self.tail > self.head {
            self.tail - self.head
        } else {
            self.tail + RING_BUFFER_SIZE - self.head
        }
    }
    pub fn available_write(&self) -> usize {
        if self.status == RingBufferStatus::Full {
            0
        } else {
            RING_BUFFER_SIZE - self.available_read()
        }
    }
    /// check if all write ends are closed
    pub fn all_write_ends_closed(&self) -> bool {
        self.write_end.as_ref().unwrap().upgrade().is_none()
    }
}

impl FileIO for SocketPair {
    fn read(&self, buf: &mut [u8]) -> AxResult<usize> {
        info!("kernel: Pipe::read");
        assert!(self.readable());
        let want_to_read = buf.len();
        let mut buf_iter = buf.iter_mut();
        let mut already_read = 0usize;
        loop {
            let mut ring_buffer = self.peer_buffer.lock();
            let loop_read = ring_buffer.available_read();
            info!("kernel: Pipe::read: loop_read = {}", loop_read);
            if loop_read == 0 {
                if Arc::strong_count(&self.self_buffer) < 2
                // write end closed
                    || ring_buffer.all_write_ends_closed()
                    || self.non_block
                {
                    return Ok(already_read);
                }
                drop(ring_buffer);
                yield_now();
                continue;
            }
            for _ in 0..loop_read {
                if let Some(byte_ref) = buf_iter.next() {
                    *byte_ref = ring_buffer.read_byte();
                    already_read += 1;
                    if already_read == want_to_read {
                        return Ok(want_to_read);
                    }
                } else {
                    break;
                }
            }

            return Ok(already_read);
        }
    }

    fn write(&self, buf: &[u8]) -> AxResult<usize> {
        info!("kernel: Pipe::write");
        assert!(self.writable());
        let want_to_write = buf.len();
        let mut buf_iter = buf.iter();
        let mut already_write = 0usize;
        loop {
            let mut ring_buffer = self.self_buffer.lock();
            let loop_write = ring_buffer.available_write();
            if loop_write == 0 {
                drop(ring_buffer);

                if Arc::strong_count(&self.self_buffer) < 2 || self.non_block {
                    // read end closed
                }
                yield_now();
                continue;
            }

            // write at most loop_write bytes
            for _ in 0..loop_write {
                if let Some(byte_ref) = buf_iter.next() {
                    ring_buffer.write_byte(*byte_ref);
                    already_write += 1;
                    if already_write == want_to_write {
                        drop(ring_buffer);
                        return Ok(want_to_write);
                    }
                } else {
                    break;
                }
            }
            return Ok(already_write);
        }
    }

    fn executable(&self) -> bool {
        false
    }
    fn readable(&self) -> bool {
        self.readable
    }
    fn writable(&self) -> bool {
        self.writable
    }

    fn get_type(&self) -> FileIOType {
        FileIOType::Socket
    }

    fn ready_to_read(&self) -> bool {
        self.readable && self.self_buffer.lock().available_read() != 0
    }

    fn ready_to_write(&self) -> bool {
        self.writable && self.self_buffer.lock().available_write() != 0
    }
}

/// when create_pair succeed, there are two strong arc(SocketPair) and one weak arc(RingBuffer.write_end) for two UNIX socket 
pub fn create_pair(non_block: bool) -> (Arc<SocketPair>, Arc<SocketPair>) {
    let buffer1 = Arc::new(Mutex::new(SocketRingBuffer::new()));
    let buffer2 = Arc::new(Mutex::new(SocketRingBuffer::new()));

    let socket1 = Arc::new(SocketPair {
        readable: true,
        writable: true,
        peer_buffer: Arc::clone(&buffer1),
        self_buffer: Arc::clone(&buffer2),
        non_block,
    });

    let socket2 = Arc::new(SocketPair {
        readable: true,
        writable: true,
        peer_buffer: Arc::clone(&buffer2),
        self_buffer: Arc::clone(&buffer1),
        non_block,
    });

    // connect two sockets
    buffer1.lock().set_write_end(&socket2);
    buffer2.lock().set_write_end(&socket1);
    (socket1, socket2)
}
