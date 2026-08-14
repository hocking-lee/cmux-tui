//! 线程之间传递的消息。全部是不可变数据，不含共享状态。

use crate::mux::frame::Frame;
use ratatui::crossterm::event::KeyEvent;

/// 从 I/O 线程与输入线程流向主循环。
#[derive(Debug)]
pub enum Event {
    /// 解出一个完整帧。
    Frame(Frame),
    /// 收到 FCS 错误帧，附原始字节。
    FcsError(Vec<u8>),
    /// 无法成帧的字节。
    Garbage(Vec<u8>),
    /// 键盘输入。
    Key(KeyEvent),
    /// 串口读写失败，链路视为断开。
    SerialError(String),
    /// 周期性心跳，驱动超时与重绘。
    Tick,
}

/// 从主循环流向写线程。
#[derive(Debug)]
pub enum TxRequest {
    /// 发送一个已构造的帧。
    Frame(Frame),
    /// 直接写原始字节（AT 阶段使用）。
    Raw(Vec<u8>),
    /// 请求写线程退出。
    Shutdown,
}
