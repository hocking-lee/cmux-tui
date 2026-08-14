//! 会话日志落盘。文本格式，便于事后 grep 与回放。

use crate::app::record::{Record, hexdump_lines};
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;

pub struct SessionLog {
    out: BufWriter<std::fs::File>,
}

impl SessionLog {
    /// 以追加方式打开日志文件，并写入一行会话头。
    pub fn open(path: &Path) -> std::io::Result<SessionLog> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut out = BufWriter::new(file);
        writeln!(
            out,
            "==== cmux-tui 会话日志 {} ====",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        )?;
        Ok(SessionLog { out })
    }

    /// 写入一条记录：一行头 + 若干 hex/ascii 行。
    pub fn write_record(&mut self, rec: &Record) -> std::io::Result<()> {
        writeln!(
            self.out,
            "{} DLCI {} {}",
            rec.ts.format("%H:%M:%S%.3f"),
            rec.dlci,
            match &rec.note {
                Some(n) => format!("{} {}", rec.dir.label(), n),
                None => format!("{} {}B", rec.dir.label(), rec.bytes.len()),
            }
        )?;
        for (hex, ascii) in hexdump_lines(&rec.bytes, 16) {
            writeln!(self.out, "    {hex}  |{ascii}|")?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::record::{Direction, Record};

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("cmux-log-test-{name}-{}.log", std::process::id()));
        p
    }

    #[test]
    fn writes_header_on_open() {
        let path = tmp_path("header");
        let _ = std::fs::remove_file(&path);
        {
            let mut l = SessionLog::open(&path).unwrap();
            l.flush().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("cmux-tui 会话日志"), "实际: {text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn appends_one_line_per_record_with_hex_and_ascii() {
        let path = tmp_path("records");
        let _ = std::fs::remove_file(&path);
        {
            let mut l = SessionLog::open(&path).unwrap();
            l.write_record(&Record::new(Direction::Tx, 1, b"AT\r".to_vec()))
                .unwrap();
            l.write_record(&Record::new(Direction::Rx, 1, b"OK".to_vec()))
                .unwrap();
            l.flush().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("DLCI 1 TX"), "实际: {text}");
        assert!(text.contains("41 54 0D"), "应含 hex，实际: {text}");
        assert!(text.contains("|AT.|"), "应含 ascii，实际: {text}");
        assert!(text.contains("4F 4B"), "实际: {text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn info_records_log_their_note() {
        let path = tmp_path("info");
        let _ = std::fs::remove_file(&path);
        {
            let mut l = SessionLog::open(&path).unwrap();
            l.write_record(&Record::info(1, "建链成功")).unwrap();
            l.flush().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("建链成功"), "实际: {text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn opening_existing_file_appends_rather_than_truncates() {
        let path = tmp_path("append");
        let _ = std::fs::remove_file(&path);
        {
            let mut l = SessionLog::open(&path).unwrap();
            l.write_record(&Record::info(1, "第一段")).unwrap();
            l.flush().unwrap();
        }
        {
            let mut l = SessionLog::open(&path).unwrap();
            l.write_record(&Record::info(1, "第二段")).unwrap();
            l.flush().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("第一段") && text.contains("第二段"),
            "实际: {text}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_fails_on_unwritable_path() {
        assert!(SessionLog::open(std::path::Path::new("/proc/nonexistent-dir/x.log")).is_err());
    }
}
