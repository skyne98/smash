use anyhow::Result;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use tui_term::vt100;

pub struct Terminal {
    pub parser: Arc<Mutex<vt100::Parser>>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Terminal {
    pub fn new(rows: u16, cols: u16) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let shell = if cfg!(windows) { "cmd.exe" } else { "bash" };
        let cmd = CommandBuilder::new(shell);
        pair.slave.spawn_command(cmd)?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let parser_clone = Arc::clone(&parser);
        let mut reader = pair.master.try_clone_reader()?;

        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut parser = parser_clone.lock().unwrap();
                parser.process(&buf[..n]);
            }
        });

        let writer = Arc::new(Mutex::new(pair.master.take_writer()?));

        Ok(Self {
            parser,
            master: pair.master,
            writer,
        })
    }

    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        // Note: vt100 parser doesn't easily resize, 
        // we might need to recreate it if needed, but for now we'll keep the old one.
        Ok(())
    }
}
