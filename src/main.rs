//! cmux-tui：CMUX (3GPP TS 27.010) 串口调试工具。
//!
//! 本二进制只负责装配：解析参数、打开串口、做 AT 握手、启动线程、
//! 跑主循环、退出时恢复终端。协议与界面逻辑都在库里。

use clap::Parser;
use cmux_tui::app::event::Event;
use cmux_tui::app::io_threads::{spawn_input, spawn_reader, spawn_writer};
use cmux_tui::app::keymap::{Command, map_key};
use cmux_tui::app::runtime::Runtime;
use cmux_tui::app::state::{AppConfig, LinkState};
use cmux_tui::mux::session::SessionConfig;
use cmux_tui::serial::at::{AtConfig, enter_mux};
use cmux_tui::serial::stream::{ByteStream, SerialStream, list_ports};
use cmux_tui::ui;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 进入帧模式后的串口读超时。取值要小，否则退出时读线程回收会明显卡顿。
const FRAME_MODE_READ_TIMEOUT: Duration = Duration::from_millis(50);

#[derive(Parser, Debug)]
#[command(name = "cmux-tui", about = "CMUX (27.010) 串口调试工具", version)]
struct Cli {
    /// 串口设备路径
    #[arg(short, long)]
    port: Option<String>,

    /// 波特率
    #[arg(short, long, default_value_t = 115200)]
    baud: u32,

    /// 启动时自动建链的 DLCI，逗号分隔，最多 2 个
    #[arg(long, value_delimiter = ',')]
    dlci: Vec<u8>,

    /// 跳过 AT 握手，假定对端已处于复用模式
    #[arg(long)]
    no_at: bool,

    /// 启动即开启会话日志
    #[arg(long)]
    log: Option<PathBuf>,

    /// 最大帧信息长度 N1
    #[arg(long, default_value_t = 127)]
    n1: u16,

    /// ASCII 模式发送时不自动追加 \r
    #[arg(long)]
    no_cr: bool,

    /// 列出可用串口后退出
    #[arg(long)]
    list_ports: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.list_ports {
        let ports = list_ports();
        if ports.is_empty() {
            println!("未发现串口");
        } else {
            for p in ports {
                println!("{p}");
            }
        }
        return Ok(());
    }

    if cli.dlci.len() > 2 {
        anyhow::bail!("最多同时观察 2 个子通道，收到 {} 个", cli.dlci.len());
    }
    if let Some(bad) = cli.dlci.iter().find(|d| !(1..=63).contains(*d)) {
        anyhow::bail!("DLCI 必须在 1-63 之间，收到 {bad}");
    }

    let Some(port) = cli.port.clone() else {
        let ports = list_ports();
        anyhow::bail!(
            "请用 --port 指定串口。当前可用：{}",
            if ports.is_empty() {
                "（无）".to_string()
            } else {
                ports.join(", ")
            }
        );
    };

    let at_cfg = AtConfig { n1: cli.n1, ..AtConfig::default() };

    // 握手阶段用较长的读超时：at 模块把一次读超时当作「本轮无响应」。
    let mut stream = SerialStream::open(&port, cli.baud, at_cfg.timeout)
        .map_err(|e| anyhow::anyhow!("打开 {port} 失败：{e}"))?;

    // AT 握手必须在启动读线程之前做，避免两处同时读串口。
    let mut link = LinkState::Up;
    let mut handshake_note = None;
    if !cli.no_at
        && let Err(e) = enter_mux(&mut stream, &at_cfg)
    {
        link = LinkState::Down;
        handshake_note = Some(format!("AT 握手失败：{e}（可用 --no-at 跳过）"));
    }

    // 进入帧模式，改回短超时。
    stream.set_timeout(FRAME_MODE_READ_TIMEOUT)?;

    let reader_stream: Box<dyn ByteStream> = stream.try_clone_stream()?;
    let writer_stream: Box<dyn ByteStream> = Box::new(stream);

    let (ev_tx, ev_rx) = mpsc::channel::<Event>();
    let reader = spawn_reader(reader_stream, ev_tx.clone(), cli.n1 as usize);
    let (tx_req, writer) = spawn_writer(writer_stream);
    let input = spawn_input(ev_tx, Duration::from_millis(60));

    let app_cfg = AppConfig { append_cr: !cli.no_cr, ..AppConfig::default() };
    let mut rt = Runtime::new(app_cfg, SessionConfig::default(), tx_req);
    rt.state.port_desc = format!("{port} {} 8N1", cli.baud);
    rt.state.link = link;
    rt.state.notice = handshake_note;
    rt.log_path = cli.log.clone();
    if cli.log.is_some() {
        rt.exec(Command::ToggleLog, Instant::now());
    }

    // 进入 TUI 前装 panic hook，保证异常时终端不会残留在 raw 模式。
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        default_hook(info);
    }));

    let mut terminal = ratatui::init();

    // 启动时自动建链。
    for (slot, dlci) in cli.dlci.iter().enumerate() {
        rt.exec(Command::OpenDlc { dlci: *dlci, slot }, Instant::now());
    }

    let result = run_loop(&mut terminal, &mut rt, ev_rx);

    rt.shutdown(Instant::now());
    ratatui::restore();
    reader.stop();
    input.stop();
    writer.join();

    result
}

fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    rt: &mut Runtime,
    ev_rx: mpsc::Receiver<Event>,
) -> anyhow::Result<()> {
    terminal.draw(|f| ui::layout::draw(f, &rt.state))?;

    while rt.running {
        let Ok(ev) = ev_rx.recv() else { break };
        let now = Instant::now();
        dispatch(rt, ev, now);

        // 把队列里已到达的事件一次性处理完再重绘，避免高流量下刷屏。
        while let Ok(ev) = ev_rx.try_recv() {
            dispatch(rt, ev, now);
            if !rt.running {
                break;
            }
        }

        terminal.draw(|f| ui::layout::draw(f, &rt.state))?;
    }
    Ok(())
}

/// 按键先经 keymap 翻译成命令，其余事件直接交给运行时。
fn dispatch(rt: &mut Runtime, ev: Event, now: Instant) {
    match ev {
        Event::Key(k) => {
            let cmd = map_key(&mut rt.state, k);
            rt.exec(cmd, now);
        }
        other => rt.on_event(other, now),
    }
}
