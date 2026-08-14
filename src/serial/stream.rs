//! 串口的可测试抽象。
//!
//! 协议层只依赖 `ByteStream`，因此帧编解码、AT 握手、会话状态机
//! 全部可以用 `MockStream` 在内存中驱动，无需真实硬件。

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 可读可写、可克隆出独立句柄的字节流。
///
/// 克隆出的句柄与原句柄指向同一底层设备，用于让读写分处两条线程。
pub trait ByteStream: Read + Write + Send {
    fn try_clone_stream(&self) -> io::Result<Box<dyn ByteStream>>;
}

/// 真实串口。
pub struct SerialStream {
    inner: Box<dyn serialport::SerialPort>,
}

impl SerialStream {
    /// 按 8N1 无流控打开串口。`timeout` 决定读阻塞上限。
    pub fn open(
        path: &str,
        baud: u32,
        timeout: Duration,
    ) -> Result<SerialStream, serialport::Error> {
        let inner = serialport::new(path, baud)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .timeout(timeout)
            .open()?;
        Ok(SerialStream { inner })
    }

    /// 调整读超时。
    ///
    /// AT 握手期间需要较长的读超时（一次读就覆盖整轮等待），
    /// 进入帧模式后则要改短，否则退出时读线程回收会明显卡顿。
    pub fn set_timeout(&mut self, timeout: Duration) -> Result<(), serialport::Error> {
        self.inner.set_timeout(timeout)
    }
}

impl Read for SerialStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for SerialStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl ByteStream for SerialStream {
    fn try_clone_stream(&self) -> io::Result<Box<dyn ByteStream>> {
        let cloned = self
            .inner
            .try_clone()
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Box::new(SerialStream { inner: cloned }))
    }
}

/// 列出系统可用串口路径。
///
/// 注意：本 crate 关闭了 serialport 的 libudev feature，
/// 此处走 `/sys/class/tty` 扫描路径，以保证静态链接。
pub fn list_ports() -> Vec<String> {
    match serialport::available_ports() {
        Ok(ports) => ports.into_iter().map(|p| p.port_name).collect(),
        Err(_) => Vec::new(),
    }
}

#[derive(Default)]
struct MockInner {
    /// 待读取的脚本。`None` 表示这一次读返回超时，用于编排「对端沉默」。
    to_read: VecDeque<Option<Vec<u8>>>,
    written: Vec<u8>,
}

/// 内存中的 `ByteStream` 替身，用于测试。
#[derive(Clone, Default)]
pub struct MockStream {
    inner: Arc<Mutex<MockInner>>,
}

impl MockStream {
    pub fn new() -> MockStream {
        MockStream::default()
    }

    /// 排入一段「将被读到」的字节。每次 `read` 消费一个 feed 块。
    pub fn feed(&self, bytes: &[u8]) {
        self.inner
            .lock()
            .unwrap()
            .to_read
            .push_back(Some(bytes.to_vec()));
    }

    /// 排入一次读超时，用于模拟对端在这一轮保持沉默。
    pub fn feed_timeout(&self) {
        self.inner.lock().unwrap().to_read.push_back(None);
    }

    /// 取出迄今为止写入的全部字节。
    pub fn written(&self) -> Vec<u8> {
        self.inner.lock().unwrap().written.clone()
    }

    /// 清空写入记录，便于分阶段断言。
    pub fn clear_written(&self) {
        self.inner.lock().unwrap().written.clear();
    }
}

impl Read for MockStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut g = self.inner.lock().unwrap();
        match g.to_read.pop_front() {
            Some(Some(chunk)) => {
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    g.to_read.push_front(Some(chunk[n..].to_vec()));
                }
                Ok(n)
            }
            // 显式排入的一次沉默，以及脚本耗尽——都表现为读超时，
            // 与真实串口在无数据时的行为一致。
            Some(None) | None => Err(io::Error::new(io::ErrorKind::TimedOut, "no data")),
        }
    }
}

impl Write for MockStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.lock().unwrap().written.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ByteStream for MockStream {
    fn try_clone_stream(&self) -> io::Result<Box<dyn ByteStream>> {
        Ok(Box::new(self.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn mock_returns_scripted_bytes() {
        let mut s = MockStream::new();
        s.feed(b"OK\r\n");
        let mut buf = [0u8; 8];
        let n = s.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"OK\r\n");
    }

    #[test]
    fn mock_records_writes() {
        let mut s = MockStream::new();
        s.write_all(b"AT\r").unwrap();
        assert_eq!(s.written(), b"AT\r");
    }

    #[test]
    fn mock_read_times_out_when_empty() {
        let mut s = MockStream::new();
        let mut buf = [0u8; 4];
        let err = s.read(&mut buf).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    }

    #[test]
    fn mock_clone_shares_state() {
        let s = MockStream::new();
        let mut a = s.try_clone_stream().unwrap();
        a.write_all(b"x").unwrap();
        // 原句柄能看到克隆写入的内容
        assert_eq!(s.written(), b"x");
    }

    #[test]
    fn mock_can_script_a_silent_round() {
        let mut s = MockStream::new();
        s.feed_timeout();
        s.feed(b"OK");
        let mut buf = [0u8; 8];
        assert_eq!(
            s.read(&mut buf).unwrap_err().kind(),
            std::io::ErrorKind::TimedOut,
            "第一次读应表现为对端沉默"
        );
        let n = s.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"OK", "沉默之后应读到下一段脚本");
    }

    #[test]
    fn mock_reads_are_chunked_by_feed() {
        let mut s = MockStream::new();
        s.feed(b"AB");
        s.feed(b"CD");
        let mut buf = [0u8; 16];
        let n1 = s.read(&mut buf).unwrap();
        assert_eq!(&buf[..n1], b"AB");
        let n2 = s.read(&mut buf).unwrap();
        assert_eq!(&buf[..n2], b"CD");
    }
}
