//! CLI 黑盒验收：参数校验必须在进入 TUI 之前就拦住错误输入。

use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cmux-tui")
}

#[test]
fn help_lists_the_documented_flags() {
    let out = Command::new(bin()).arg("--help").output().expect("运行失败");
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in [
        "--port",
        "--baud",
        "--dlci",
        "--no-at",
        "--log",
        "--n1",
        "--no-cr",
        "--list-ports",
    ] {
        assert!(text.contains(flag), "帮助缺少 {flag}:\n{text}");
    }
}

#[test]
fn rejects_more_than_two_dlcis() {
    let out = Command::new(bin())
        .args(["--port", "/dev/null", "--dlci", "1,3,5"])
        .output()
        .expect("运行失败");
    assert!(!out.status.success(), "三个 DLCI 应被拒绝");
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("最多"), "实际: {text}");
}

#[test]
fn rejects_out_of_range_dlci() {
    let out = Command::new(bin())
        .args(["--port", "/dev/null", "--dlci", "99"])
        .output()
        .expect("运行失败");
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("1-63"), "实际: {text}");
}

#[test]
fn missing_port_is_reported_with_available_ports() {
    let out = Command::new(bin()).output().expect("运行失败");
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("--port"), "实际: {text}");
}

#[test]
fn list_ports_exits_cleanly() {
    let out = Command::new(bin())
        .arg("--list-ports")
        .output()
        .expect("运行失败");
    assert!(out.status.success(), "--list-ports 应正常退出");
}
