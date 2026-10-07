//! Reference editor-side wire client (P1).
//!
//! This module implements the editor side of the Oxipresso/TeXpresso wire —
//! the integration contract an editor plugin implements — as a reusable
//! Rust library: spawn an `oxipresso` binary, speak the editor commands over
//! stdin, parse the engine's notices from stdout, and drive full
//! initialize/rebuild/pause-resume cycles.
//!
//! Two consumers:
//! - `run_wire_selftest` — a headless contract check that the shipped
//!   binary speaks the expected message shapes from OUTSIDE the Rust
//!   process (pinned by a cargo test against `CARGO_BIN_EXE_oxipresso`).
//! - the `oxipresso-editor-client` GUI binary (Slint), a small editor
//!   window wired to a live engine process.

use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc::Receiver},
    time::{Duration, Instant},
};

use oxipresso_editor_protocol::{InfoBuffer, LookupKind, LookupStatus, WireProtocol};

/// One engine→editor notice, parsed from a wire line. Mirrors
/// `EditorMessage`'s serialization forms without owning them (the parser is
/// intentionally independent of the serializer so a wire regression cannot
/// hide behind a shared type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireNotice {
    Truncate {
        buffer: InfoBuffer,
        lines: bool,
        amount: usize,
    },
    Append {
        buffer: InfoBuffer,
        lines: bool,
        pos: Option<usize>,
        text: String,
    },
    Flush,
    InputFile {
        index: usize,
        path: String,
    },
    LookupFile {
        kind: LookupKind,
        status: LookupStatus,
        path: String,
    },
    Synctex {
        path: String,
        line: usize,
        column: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNotice {
    pub raw: String,
    pub notice: WireNotice,
}

// ---------------------------------------------------------------------------
// minimal s-expression parsing (engine→editor direction)

#[derive(Debug, Clone)]
enum Field {
    List(Vec<Field>),
    Str(String),
    Atom(String),
}

impl Field {
    fn as_str(&self) -> Option<&str> {
        match self {
            Field::Str(s) => Some(s),
            _ => None,
        }
    }

    fn as_atom(&self) -> Option<&str> {
        match self {
            Field::Atom(a) => Some(a),
            _ => None,
        }
    }

    fn as_int(&self) -> Option<usize> {
        match self {
            Field::Atom(a) => a.parse().ok(),
            _ => None,
        }
    }
}

fn parse_fields(line: &str) -> Result<Vec<Field>, String> {
    let mut stack: Vec<Vec<Field>> = vec![Vec::new()];
    let bytes: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            '(' => {
                stack.push(Vec::new());
                i += 1;
            }
            ')' => {
                let done = stack.pop().ok_or("unbalanced ')'")?;
                stack
                    .last_mut()
                    .ok_or("unbalanced ')'")?
                    .push(Field::List(done));
                i += 1;
            }
            '"' => {
                i += 1;
                let mut buf = String::new();
                let mut closed = false;
                while i < bytes.len() {
                    match bytes[i] {
                        '\\' if i + 1 < bytes.len() => {
                            buf.push(match bytes[i + 1] {
                                'n' => '\n',
                                't' => '\t',
                                other => other,
                            });
                            i += 2;
                        }
                        '"' => {
                            closed = true;
                            i += 1;
                            break;
                        }
                        c => {
                            buf.push(c);
                            i += 1;
                        }
                    }
                }
                if !closed {
                    return Err("unterminated string literal".to_string());
                }
                stack
                    .last_mut()
                    .ok_or("string outside expression")?
                    .push(Field::Str(buf));
            }
            c if c.is_whitespace() => i += 1,
            _ => {
                let start = i;
                while i < bytes.len()
                    && !bytes[i].is_whitespace()
                    && bytes[i] != '('
                    && bytes[i] != ')'
                    && bytes[i] != '"'
                {
                    i += 1;
                }
                stack
                    .last_mut()
                    .ok_or("atom outside expression")?
                    .push(Field::Atom(bytes[start..i].iter().collect::<String>()));
            }
        }
    }
    if stack.len() != 1 {
        return Err("unbalanced '('".to_string());
    }
    let root = stack.pop().unwrap();
    // The engine sends exactly one top-level list per line; unwrap the root
    // list so notice parsing sees the fields directly.
    if root.len() == 1
        && let Field::List(items) = &root[0]
    {
        return Ok(items.clone());
    }
    Ok(root)
}

fn parse_buffer(name: &str) -> Option<InfoBuffer> {
    match name {
        "out" => Some(InfoBuffer::Out),
        "log" => Some(InfoBuffer::Log),
        _ => None,
    }
}

/// Parse one engine→editor wire line into a notice. Returns `None` for
/// lines the client ignores (e.g. `reset-sync`, which this fork never
/// emits).
pub fn parse_notice(line: &str) -> Option<ParsedNotice> {
    let fields = parse_fields(line).ok()?;
    let head = fields.first()?.as_atom()?.to_string();
    let notice = match head.as_str() {
        "truncate" => WireNotice::Truncate {
            buffer: parse_buffer(fields.get(1)?.as_atom()?)?,
            lines: false,
            amount: fields.get(2)?.as_int()?,
        },
        "truncate-lines" => WireNotice::Truncate {
            buffer: parse_buffer(fields.get(1)?.as_atom()?)?,
            lines: true,
            amount: fields.get(2)?.as_int()?,
        },
        "append" => WireNotice::Append {
            buffer: parse_buffer(fields.get(1)?.as_atom()?)?,
            lines: false,
            pos: fields.get(2)?.as_int(),
            text: fields.get(3)?.as_str()?.to_string(),
        },
        "append-lines" => WireNotice::Append {
            buffer: parse_buffer(fields.get(1)?.as_atom()?)?,
            lines: true,
            pos: None,
            text: fields[2..]
                .iter()
                .filter_map(|f| f.as_str().map(str::to_string))
                .collect::<Vec<_>>()
                .join("\n"),
        },
        "flush" => WireNotice::Flush,
        "input-file" => WireNotice::InputFile {
            index: fields.get(1)?.as_int()?,
            path: fields.get(2)?.as_str()?.to_string(),
        },
        "lookup-file" => WireNotice::LookupFile {
            kind: match fields.get(1)?.as_atom()? {
                "read" => LookupKind::Read,
                "write" => LookupKind::Write,
                _ => return None,
            },
            status: match fields.get(2)?.as_atom()? {
                "successful" => LookupStatus::Successful,
                "failed" => LookupStatus::Failed,
                "promised" => LookupStatus::Promised,
                _ => return None,
            },
            path: fields.get(3)?.as_str()?.to_string(),
        },
        "synctex" => WireNotice::Synctex {
            path: fields.get(1)?.as_str()?.to_string(),
            line: fields.get(2)?.as_int()?,
            column: fields.get(3)?.as_int()?,
        },
        _ => return None,
    };
    Some(ParsedNotice {
        raw: line.to_string(),
        notice,
    })
}

/// Escape a Rust string into a wire string literal.
pub fn escape_wire_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// the live session

/// A running `oxipresso` child the client talks to over stdin/stdout.
pub struct EditorWireSession {
    child: Child,
    stdin: ChildStdin,
    notices: Receiver<ParsedNotice>,
    transcript: Arc<Mutex<Vec<String>>>,
    stderr: Arc<Mutex<String>>,
    protocol: WireProtocol,
}

impl EditorWireSession {
    /// Spawn `binary` on `root` and start the stdout reader thread.
    pub fn spawn(
        binary: &Path,
        root: &Path,
        protocol: WireProtocol,
        stream: bool,
    ) -> Result<Self, String> {
        let mut args: Vec<String> = Vec::new();
        if protocol == WireProtocol::Json {
            args.push("-json".to_string());
        }
        if stream {
            args.push("-stream".to_string());
        }
        args.push(root.to_string_lossy().into_owned());
        let mut child = Command::new(binary)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn {}: {e}", binary.display()))?;
        let stdin = child.stdin.take().ok_or("child stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("child stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("child stderr unavailable")?;
        let transcript: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let stderr_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let (tx, rx) = std::sync::mpsc::channel::<ParsedNotice>();
        let transcript_for_reader = Arc::clone(&transcript);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                {
                    let mut t = transcript_for_reader
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    t.push(line.clone());
                }
                if let Some(parsed) = parse_notice(&line)
                    && tx.send(parsed).is_err()
                {
                    break;
                }
            }
        });
        let stderr_for_reader = Arc::clone(&stderr_buf);
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let mut buf = stderr_for_reader.lock().unwrap_or_else(|p| p.into_inner());
                buf.push_str(&line);
                buf.push('\n');
            }
        });
        Ok(Self {
            child,
            stdin,
            notices: rx,
            transcript,
            stderr: stderr_buf,
            protocol,
        })
    }

    pub fn protocol(&self) -> WireProtocol {
        self.protocol
    }

    /// All wire lines seen so far (raw).
    pub fn transcript(&self) -> Vec<String> {
        self.transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn stderr_text(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Send one raw wire line (already formatted).
    pub fn send_raw(&mut self, line: &str) -> Result<(), String> {
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("wire send failed: {e}"))
    }

    /// `(open-base64 "path" "...")` — register editor content for a file.
    pub fn open_document(&mut self, path: &str, content: &[u8]) -> Result<(), String> {
        use base64::Engine as _;
        let payload = base64::engine::general_purpose::STANDARD.encode(content);
        let line = if self.protocol == WireProtocol::Json {
            format!(
                "[\"open-base64\",{},\"{payload}\"]",
                serde_json_string(path)?
            )
        } else {
            format!("(open-base64 {} \"{payload}\")", escape_wire_string(path))
        };
        self.send_raw(&line)
    }

    /// `(change "path" OFFSET REMOVE INSERT)` — the whole-buffer replace an
    /// editor sends when the user saves.
    pub fn change(
        &mut self,
        path: &str,
        offset: usize,
        remove: usize,
        insert: &str,
    ) -> Result<(), String> {
        let line = if self.protocol == WireProtocol::Json {
            format!(
                "[\"change\",{},{},{},{}]",
                serde_json_string(path)?,
                offset,
                remove,
                serde_json_string(insert)?
            )
        } else {
            format!(
                "(change {} {offset} {remove} {})",
                escape_wire_string(path),
                escape_wire_string(insert)
            )
        };
        self.send_raw(&line)
    }

    pub fn pause(&mut self) -> Result<(), String> {
        self.send_raw("(pause)")
    }

    pub fn resume(&mut self) -> Result<(), String> {
        self.send_raw("(resume)")
    }

    pub fn rescan(&mut self) -> Result<(), String> {
        self.send_raw("(rescan)")
    }

    /// Next notice (blocks up to `timeout`); `None` on timeout or engine exit.
    pub fn next_notice(&mut self, timeout: Duration) -> Option<ParsedNotice> {
        self.notices.recv_timeout(timeout).ok()
    }

    /// Collect notices until one satisfies `predicate` (returned); `None` on
    /// timeout. Everything seen stays on the transcript.
    pub fn wait_notice(
        &mut self,
        predicate: impl Fn(&WireNotice) -> bool,
        timeout: Duration,
    ) -> Option<ParsedNotice> {
        let deadline = Instant::now() + timeout;
        while let Some(parsed) =
            self.next_notice(deadline.saturating_duration_since(Instant::now()))
        {
            if predicate(&parsed.notice) {
                return Some(parsed);
            }
        }
        None
    }

    /// Terminate the child (the editor closed the session).
    pub fn shutdown(&mut self) {
        let _ = self.stdin.write_all(b"(pause)\n");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for EditorWireSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Minimal JSON string encoding for the `-json` wire form (quotes/escapes
/// only; control characters do not appear in the paths and texts we send).
fn serde_json_string(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                return Err(format!(
                    "control character not supported on the json wire: {c:?}"
                ));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

// ---------------------------------------------------------------------------
// the headless contract check

/// Drive a full initialize → change-rebuild → pause/resume cycle against the
/// shipped binary and assert the message shapes. Returns Err describing the
/// first broken expectation — the same contract the GUI client relies on.
pub fn run_wire_selftest(binary: &Path) -> Result<(), String> {
    let temp_dir: PathBuf = std::env::temp_dir().join(format!(
        "oxi-wire-selftest-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let doc_path = temp_dir.join("main.tex");
    let doc = "\\documentclass{article}\n\\begin{document}\nClient\n\\end{document}\n";
    std::fs::write(&doc_path, doc).map_err(|e| e.to_string())?;

    let mut session = EditorWireSession::spawn(binary, &doc_path, WireProtocol::Sexp, false)?;

    let result = (|| -> Result<(), String> {
        let first = session
            .next_notice(Duration::from_secs(60))
            .ok_or("no initial notice: the engine did not speak")?;
        match first.notice {
            WireNotice::Truncate {
                buffer: InfoBuffer::Out,
                lines: false,
                amount: 0,
            } => {}
            other => return Err(format!("expected (truncate out 0) first, got {other:?}")),
        }
        session
            .wait_notice(|n| matches!(n, WireNotice::Flush), Duration::from_secs(60))
            .ok_or("the initialization stream never flushed")?;
        let input = session
            .wait_notice(
                |n| matches!(n, WireNotice::InputFile { .. }),
                Duration::from_secs(60),
            )
            .ok_or("no input-file notification after initialization")?;
        match input.notice {
            WireNotice::InputFile { path, .. } if path.ends_with("main.tex") => {}
            other => return Err(format!("input-file did not name the root: {other:?}")),
        }

        // Hot rebuild: a change produces a fresh full stream.
        let offset = doc.find("Client").ok_or("selftest doc missing marker")?;
        session.change("main.tex", offset, "Client".len(), "Edit0r")?;
        session
            .wait_notice(
                |n| {
                    matches!(
                        n,
                        WireNotice::Truncate {
                            buffer: InfoBuffer::Out,
                            lines: false,
                            amount: 0
                        }
                    )
                },
                Duration::from_secs(60),
            )
            .ok_or("a change did not trigger a fresh out truncate")?;
        session
            .wait_notice(|n| matches!(n, WireNotice::Flush), Duration::from_secs(60))
            .ok_or("the rebuild stream never flushed")?;

        // Pause/resume keeps the session alive and folds into a rebuild.
        session.pause()?;
        session.resume()?;
        session
            .wait_notice(
                |n| {
                    matches!(
                        n,
                        WireNotice::Truncate {
                            buffer: InfoBuffer::Out,
                            lines: false,
                            amount: 0
                        }
                    )
                },
                Duration::from_secs(60),
            )
            .ok_or("resume did not rebuild after a paused span")?;
        session
            .wait_notice(|n| matches!(n, WireNotice::Flush), Duration::from_secs(60))
            .ok_or("the resumed stream never flushed")?;
        Ok(())
    })();

    session.shutdown();
    let transcript = session.transcript();
    let stderr_text = session.stderr_text();
    let _ = std::fs::remove_dir_all(&temp_dir);
    result.map_err(|error| {
        let mut message = format!("{error}\n---- wire transcript (first 12) ----\n");
        for line in transcript.iter().take(12) {
            message.push_str(&format!("  {}\n", &line[..line.len().min(160)]));
        }
        let stderr_head: String = stderr_text.lines().take(6).collect::<Vec<_>>().join("\n");
        if !stderr_head.is_empty() {
            message.push_str("---- engine stderr (first 6) ----\n");
            for line in stderr_head.lines() {
                message.push_str(&format!("  {}\n", &line[..line.len().min(160)]));
            }
        }
        message
    })
}
