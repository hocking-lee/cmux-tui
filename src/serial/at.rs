//! 进入 CMUX 复用模式前的 AT 握手。
//!
//! 流程：反复发 `AT` 直到收到 `OK`（确认串口通、模块在命令模式），
//! 然后发 `AT+CMUX=...`，收到 `OK` 后对端即切换到帧模式。
//!
//! **对调用方的契约**：本模块把「一次读超时」当作「对端这一轮没有响应」，
//! 因此调用前必须把串口的读超时设为不小于 `AtConfig::timeout`
//! （见 `SerialStream::set_timeout`）。若读超时远小于整轮超时，
//! 工具会在模块回复到达之前就判定失败并重发。

use crate::serial::stream::ByteStream;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct AtConfig {
    /// `AT` 探测的最大次数。
    pub attempts: u8,
    /// 单次等待响应的超时。
    pub timeout: Duration,
    /// 协商的最大帧信息长度 N1。
    pub n1: u16,
}

impl Default for AtConfig {
    fn default() -> Self {
        AtConfig { attempts: 3, timeout: Duration::from_secs(1), n1: 127 }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AtError {
    #[error("模块无响应")]
    NoResponse,
    #[error("模块拒绝: {0}")]
    Rejected(String),
    #[error("串口错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 执行完整握手。成功返回时对端已处于帧模式。
pub fn enter_mux(stream: &mut dyn ByteStream, cfg: &AtConfig) -> Result<(), AtError> {
    let mut ok = false;
    for _ in 0..cfg.attempts {
        stream.write_all(b"AT\r")?;
        stream.flush()?;
        match read_response(stream, cfg.timeout)? {
            AtReply::Ok => {
                ok = true;
                break;
            }
            AtReply::Error(_) | AtReply::Timeout => continue,
        }
    }
    if !ok {
        return Err(AtError::NoResponse);
    }

    // 参数依次为：Basic 模式、无子集、波特率档位 5、N1、
    // T1=100ms、N2=3、T2=300ms、T3=10s、k=2
    let cmd = format!("AT+CMUX=0,0,5,{},10,3,30,10,2\r", cfg.n1);
    stream.write_all(cmd.as_bytes())?;
    stream.flush()?;

    match read_response(stream, cfg.timeout)? {
        AtReply::Ok => Ok(()),
        AtReply::Error(text) => Err(AtError::Rejected(text)),
        AtReply::Timeout => Err(AtError::NoResponse),
    }
}

enum AtReply {
    Ok,
    Error(String),
    Timeout,
}

/// 读取直到出现 OK / ERROR，或本轮判定无响应。
fn read_response(stream: &mut dyn ByteStream, timeout: Duration) -> Result<AtReply, AtError> {
    let deadline = Instant::now() + timeout;
    let mut acc = String::new();
    let mut buf = [0u8; 256];

    while Instant::now() < deadline {
        match stream.read(&mut buf) {
            Ok(0) => continue,
            Ok(n) => {
                acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                if acc.contains("OK") {
                    return Ok(AtReply::Ok);
                }
                if acc.contains("ERROR") {
                    return Ok(AtReply::Error(acc.trim().to_string()));
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                // 见模块头部契约：读超时即本轮无响应。
                return Ok(AtReply::Timeout);
            }
            Err(e) => return Err(AtError::Io(e)),
        }
    }
    Ok(AtReply::Timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serial::stream::MockStream;
    use std::time::Duration;

    fn fast() -> AtConfig {
        AtConfig { attempts: 3, timeout: Duration::from_millis(50), n1: 127 }
    }

    #[test]
    fn sends_at_then_cmux_when_module_replies_ok() {
        let s = MockStream::new();
        s.feed(b"\r\nOK\r\n");
        s.feed(b"\r\nOK\r\n");
        let mut stream = s.clone();

        enter_mux(&mut stream, &fast()).expect("握手应成功");

        let written = String::from_utf8_lossy(&s.written()).to_string();
        assert!(written.starts_with("AT\r"), "首先应发 AT，实际: {written:?}");
        assert!(
            written.contains("AT+CMUX=0,0,5,127,10,3,30,10,2\r"),
            "实际: {written:?}"
        );
    }

    #[test]
    fn retries_at_when_first_probe_is_silent() {
        let s = MockStream::new();
        s.feed_timeout(); // 第一次探测：模块沉默
        s.feed(b"\r\nOK\r\n"); // 第二次探测：回 OK
        s.feed(b"\r\nOK\r\n"); // AT+CMUX 回 OK
        let mut stream = s.clone();

        enter_mux(&mut stream, &fast()).expect("重试后应成功");

        let written = String::from_utf8_lossy(&s.written()).to_string();
        assert_eq!(
            written.matches("AT\r").count(),
            2,
            "沉默一次后应重发一次 AT，实际: {written:?}"
        );
    }

    #[test]
    fn fails_when_module_never_answers() {
        let s = MockStream::new();
        let mut stream = s.clone();
        let err = enter_mux(&mut stream, &fast()).unwrap_err();
        assert!(matches!(err, AtError::NoResponse));
        // 放弃前应把 attempts 次探测都发出去
        let written = String::from_utf8_lossy(&s.written()).to_string();
        assert_eq!(written.matches("AT\r").count(), 3);
    }

    #[test]
    fn fails_when_module_returns_error() {
        let s = MockStream::new();
        s.feed(b"\r\nOK\r\n");
        s.feed(b"\r\nERROR\r\n");
        let mut stream = s.clone();
        let err = enter_mux(&mut stream, &fast()).unwrap_err();
        assert!(matches!(err, AtError::Rejected(_)), "实际: {err:?}");
    }

    #[test]
    fn n1_is_reflected_in_cmux_command() {
        let s = MockStream::new();
        s.feed(b"\r\nOK\r\n");
        s.feed(b"\r\nOK\r\n");
        let mut stream = s.clone();
        let cfg = AtConfig { n1: 64, ..fast() };
        enter_mux(&mut stream, &cfg).unwrap();
        let written = String::from_utf8_lossy(&s.written()).to_string();
        assert!(written.contains("AT+CMUX=0,0,5,64,"), "实际: {written:?}");
    }
}
