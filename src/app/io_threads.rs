//! 串口读写线程。
//!
//! 读线程：阻塞读 → 增量解帧 → 把事件发给主循环。
//! 写线程：从队列取请求 → 编码 → 写串口。
//! 两条线程各持有一个独立的串口句柄，互不阻塞。

use crate::app::event::{Event, TxRequest};
use crate::mux::decoder::{DecodeEvent, FrameDecoder};
use crate::serial::stream::ByteStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

/// 后台线程句柄，支持请求停止并等待退出。
pub struct ThreadHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ThreadHandle {
    /// 请求停止并等待线程退出。
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 写线程句柄。
pub struct WriterHandle {
    join: Option<JoinHandle<()>>,
}

impl WriterHandle {
    /// 等待线程退出。调用前应先发送 `TxRequest::Shutdown`。
    pub fn join(mut self) {
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 启动读线程。
pub fn spawn_reader(
    mut stream: Box<dyn ByteStream>,
    tx: Sender<Event>,
    max_info: usize,
) -> ThreadHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();

    let join = std::thread::Builder::new()
        .name("cmux-reader".into())
        .spawn(move || {
            let mut decoder = FrameDecoder::new(max_info);
            let mut buf = [0u8; 1024];
            loop {
                if stop_flag.load(Ordering::Relaxed) {
                    return;
                }
                match stream.read(&mut buf) {
                    Ok(0) => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Ok(n) => {
                        for ev in decoder.push(&buf[..n]) {
                            let msg = match ev {
                                DecodeEvent::Frame(f) => Event::Frame(f),
                                DecodeEvent::FcsError { raw } => Event::FcsError(raw),
                                DecodeEvent::Garbage(b) => Event::Garbage(b),
                            };
                            if tx.send(msg).is_err() {
                                return; // 主循环已退出
                            }
                        }
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::TimedOut
                            || e.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        // 读超时是常态，稍歇后重试。
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => {
                        let _ = tx.send(Event::SerialError(e.to_string()));
                        return;
                    }
                }
            }
        })
        .expect("创建读线程失败");

    ThreadHandle { stop, join: Some(join) }
}

/// 启动写线程，返回发送端与句柄。
pub fn spawn_writer(mut stream: Box<dyn ByteStream>) -> (Sender<TxRequest>, WriterHandle) {
    let (tx, rx): (Sender<TxRequest>, Receiver<TxRequest>) = mpsc::channel();

    let join = std::thread::Builder::new()
        .name("cmux-writer".into())
        .spawn(move || {
            while let Ok(req) = rx.recv() {
                let bytes = match req {
                    TxRequest::Frame(f) => f.encode(),
                    TxRequest::Raw(b) => b,
                    TxRequest::Shutdown => return,
                };
                if stream.write_all(&bytes).is_err() {
                    return;
                }
                let _ = stream.flush();
            }
        })
        .expect("创建写线程失败");

    (tx, WriterHandle { join: Some(join) })
}

/// 启动键盘输入线程，把按键转成事件并按 `tick` 周期发送心跳。
pub fn spawn_input(tx: Sender<Event>, tick: Duration) -> ThreadHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();

    let join = std::thread::Builder::new()
        .name("cmux-input".into())
        .spawn(move || {
            use ratatui::crossterm::event::{self, Event as CtEvent};
            loop {
                if stop_flag.load(Ordering::Relaxed) {
                    return;
                }
                match event::poll(tick) {
                    Ok(true) => match event::read() {
                        Ok(CtEvent::Key(k)) => {
                            if tx.send(Event::Key(k)).is_err() {
                                return;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => return,
                    },
                    Ok(false) => {
                        if tx.send(Event::Tick).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
        })
        .expect("创建输入线程失败");

    ThreadHandle { stop, join: Some(join) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::frame::{Frame, FrameType};
    use crate::serial::stream::MockStream;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn reader_thread_turns_bytes_into_frame_events() {
        let mock = MockStream::new();
        mock.feed(&Frame::sabm(1).encode());
        let (tx, rx) = mpsc::channel();

        let handle = spawn_reader(Box::new(mock), tx, 1024);

        let ev = rx.recv_timeout(Duration::from_secs(2)).expect("应收到事件");
        match ev {
            Event::Frame(f) => assert_eq!(f.ftype, FrameType::Sabm),
            other => panic!("期望 Frame，得到 {other:?}"),
        }
        handle.stop();
    }

    #[test]
    fn reader_thread_reports_fcs_errors() {
        let mock = MockStream::new();
        let mut bad = Frame::sabm(1).encode();
        let n = bad.len();
        bad[n - 2] ^= 0xFF;
        mock.feed(&bad);
        // 追一个好帧，确保坏帧被处理后线程仍在跑
        mock.feed(&Frame::sabm(2).encode());

        let (tx, rx) = mpsc::channel();
        let handle = spawn_reader(Box::new(mock), tx, 1024);

        let mut saw_err = false;
        let mut saw_frame = false;
        for _ in 0..4 {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Event::FcsError(_)) => saw_err = true,
                Ok(Event::Frame(_)) => saw_frame = true,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(saw_err, "应报告 FCS 错误");
        assert!(saw_frame, "坏帧后应继续解出好帧");
        handle.stop();
    }

    #[test]
    fn writer_thread_encodes_frames_to_the_port() {
        let mock = MockStream::new();
        let probe = mock.clone();
        let (tx, handle) = spawn_writer(Box::new(mock));

        tx.send(TxRequest::Frame(Frame::sabm(0))).unwrap();
        tx.send(TxRequest::Shutdown).unwrap();
        handle.join();

        assert_eq!(probe.written(), Frame::sabm(0).encode());
    }

    #[test]
    fn writer_thread_passes_raw_bytes_through() {
        let mock = MockStream::new();
        let probe = mock.clone();
        let (tx, handle) = spawn_writer(Box::new(mock));

        tx.send(TxRequest::Raw(b"AT\r".to_vec())).unwrap();
        tx.send(TxRequest::Shutdown).unwrap();
        handle.join();

        assert_eq!(probe.written(), b"AT\r");
    }
}
