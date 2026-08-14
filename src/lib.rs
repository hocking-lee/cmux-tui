//! cmux-tui：CMUX (3GPP TS 27.010) 串口调试工具的核心库。
//!
//! 协议、状态机与界面渲染都放在库里，二进制只负责装配与终端生命周期。
//! 这样集成测试可以直接驱动内部模块，而无需通过子进程。

pub mod app;
pub mod mux;
pub mod serial;
pub mod ui;
