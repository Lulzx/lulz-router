//! A local OpenAI *Responses* endpoint that speaks *Chat Completions* upstream.
//!
//! Codex only talks Responses (it dropped `wire_api = "chat"` in 0.14x), while
//! OpenCode Go only serves `/responses` for the OpenAI-family models. This
//! bridges the two, which is what puts qwen / minimax / kimi inside Codex.
//!
//!   codex ──Responses──▶ 127.0.0.1:PORT ──Chat Completions──▶ opencode.ai/zen/go
//!
//! Streaming is preserved end to end: upstream SSE chunks are translated and
//! flushed as they arrive, so the TUI stays live.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::thread;

use serde_json::{json, Map, Value};

/// `LULZ_DEBUG=<file>` mirrors both sides of the bridge into a log — the only
/// practical way to see what a harness actually sent.
fn debug(tag: &str, body: &str) {
    if let Ok(path) = std::env::var("LULZ_DEBUG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "[{tag}] {body}");
        }
    }
}

pub struct Upstream {
    pub base: String,
    pub key: String,
}

/// Binds an ephemeral loopback port and serves until the process exits.
pub fn spawn(up: Upstream) -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let up = Upstream { base: up.base.clone(), key: up.key.clone() };
            thread::spawn(move || {
                let _ = serve(conn, &up);
            });
        }
    });
    Ok(port)
}

fn serve(mut sock: TcpStream, up: &Upstream) -> std::io::Result<()> {
    let (path, body) = match read_request(&mut sock)? {
        Some(r) => r,
        None => return Ok(()),
    };
    if !path.ends_with("/responses") {
        return write_head(&mut sock, 404, "application/json", Some(b"{\"error\":\"not found\"}"));
    }

    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let msg = json!({"error": {"message": format!("bad request: {e}")}});
            return write_head(&mut sock, 400, "application/json", Some(msg.to_string().as_bytes()));
        }
    };
    let wants_stream = req.get("stream").and_then(Value::as_bool).unwrap_or(false);
    debug("responses-in", &req.to_string());
    let chat = to_chat(&req);
    debug("chat-out", &chat.to_string());

    // Upstream over curl: no TLS stack to vendor, and `-N` keeps the SSE live.
    // The key travels in the environment and is expanded by curl itself:
    // spelling it in argv would publish it to every `ps` on the machine.
    let mut child = Command::new("curl")
        .args(["-sS", "-N", "--max-time", "1800", "-X", "POST"])
        .args(["--variable", "%LULZ_KEY"])
        .args(["--expand-header", "Authorization: Bearer {{LULZ_KEY}}"])
        .args(["-H", "content-type: application/json", "-d", "@-"])
        .arg(format!("{}/chat/completions", up.base))
        .env("LULZ_KEY", &up.key)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(chat.to_string().as_bytes())?;
    let mut out = BufReader::new(child.stdout.take().unwrap());

    // Peek for the first payload line: a non-SSE body means the gateway
    // rejected the request, and the client deserves the real status rather
    // than a fake stream.
    let mut skipped = String::new();
    let peeked = peek_stream(&mut out, &mut skipped)?;
    debug("upstream-first", peeked.as_deref().unwrap_or(&skipped).trim());
    let Some(first) = peeked else {
        let mut rest = String::new();
        let _ = out.read_to_string(&mut rest);
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        let _ = child.wait();
        let body = format!("{skipped}{rest}");
        let body = if body.trim().is_empty() { err } else { body };
        let payload = if serde_json::from_str::<Value>(&body).is_ok() {
            body
        } else {
            json!({"error": {"message": body.trim(), "type": "upstream_error"}}).to_string()
        };
        return write_head(&mut sock, 502, "application/json", Some(payload.as_bytes()));
    };

    let mut acc = Turn::new(req.get("model").and_then(Value::as_str).unwrap_or_default());
    if wants_stream {
        write_head(&mut sock, 200, "text/event-stream", None)?;
        acc.feed(&first, &mut |e| sse(&mut sock, e))?;
        let mut line = String::new();
        while out.read_line(&mut line)? > 0 {
            acc.feed(&line, &mut |e| sse(&mut sock, e))?;
            line.clear();
        }
        acc.finish(&mut |e| sse(&mut sock, e))?;
    } else {
        let mut sink = |_: &Value| Ok(());
        acc.feed(&first, &mut sink)?;
        let mut line = String::new();
        while out.read_line(&mut line)? > 0 {
            acc.feed(&line, &mut sink)?;
            line.clear();
        }
        if let Some(msg) = acc.error() {
            let body = json!({"error": {"message": msg, "type": "upstream_error"}}).to_string();
            write_head(&mut sock, 502, "application/json", Some(body.as_bytes()))?;
        } else {
            let body = acc.response(true).to_string();
            write_head(&mut sock, 200, "application/json", Some(body.as_bytes()))?;
        }
    }
    let _ = child.wait();
    Ok(())
}

/// Reads until the first `data:` line. Blank lines, SSE comments (`: keep-alive`
/// — minimax opens with one) and the other SSE fields are not payload but they
/// do prove the response is a stream. Anything else means it is not, and what
/// was consumed lands in `skipped` so the caller can report it.
fn peek_stream<R: BufRead>(out: &mut R, skipped: &mut String) -> std::io::Result<Option<String>> {
    loop {
        let mut line = String::new();
        if out.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line.starts_with("data:") {
            return Ok(Some(line));
        }
        let t = line.trim_start();
        let is_sse_frame = t.is_empty()
            || t.starts_with(':')
            || ["event:", "id:", "retry:"].iter().any(|p| t.starts_with(p));
        skipped.push_str(&line);
        if !is_sse_frame {
            return Ok(None);
        }
    }
}

fn sse(sock: &mut TcpStream, event: &Value) -> std::io::Result<()> {
    debug("sse", &event.to_string());
    sock.write_all(format!("data: {event}\n\n").as_bytes())?;
    sock.flush()
}

fn write_head(
    sock: &mut TcpStream,
    status: u16,
    ctype: &str,
    body: Option<&[u8]>,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Bad Gateway",
    };
    let mut head = format!("HTTP/1.1 {status} {reason}\r\ncontent-type: {ctype}\r\n");
    match body {
        Some(b) => head.push_str(&format!("content-length: {}\r\nconnection: close\r\n\r\n", b.len())),
        None => head.push_str("cache-control: no-cache\r\nconnection: close\r\n\r\n"),
    }
    sock.write_all(head.as_bytes())?;
    if let Some(b) = body {
        sock.write_all(b)?;
    }
    sock.flush()
}

fn read_request(sock: &mut TcpStream) -> std::io::Result<Option<(String, Vec<u8>)>> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if sock.read(&mut byte)? == 0 {
            return Ok(None);
        }
        head.push(byte[0]);
        if head.len() > 64 * 1024 {
            return Ok(None);
        }
    }
    let head = String::from_utf8_lossy(&head).to_string();
    let path = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let len = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    sock.read_exact(&mut body)?;
    Ok(Some((path, body)))
}

// ------------------------------------------------- Responses -> Chat ------

/// Flatten a Responses request into a Chat Completions one.
pub fn to_chat(req: &Value) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    if let Some(sys) = req.get("instructions").and_then(Value::as_str) {
        if !sys.is_empty() {
            messages.push(json!({"role": "system", "content": sys}));
        }
    }

    match req.get("input") {
        Some(Value::String(s)) => messages.push(json!({"role": "user", "content": s})),
        Some(Value::Array(items)) => {
            for item in items {
                push_input_item(item, &mut messages);
            }
        }
        _ => {}
    }

    let mut body = Map::new();
    body.insert("model".into(), req.get("model").cloned().unwrap_or(Value::Null));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));

    let mut tools: Vec<Value> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for t in req.get("tools").and_then(Value::as_array).unwrap_or(&vec![]) {
        match to_chat_tool(t) {
            Some(v) => tools.push(v),
            None => dropped.push(tool_label(t)),
        }
    }
    if !dropped.is_empty() {
        debug("dropped-tools", &dropped.join(", "));
    }
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
        if let Some(tc) = req.get("tool_choice") {
            body.insert("tool_choice".into(), to_chat_tool_choice(tc));
        }
        if let Some(p) = req.get("parallel_tool_calls") {
            body.insert("parallel_tool_calls".into(), p.clone());
        }
    }
    if let Some(n) = req.get("max_output_tokens") {
        body.insert("max_tokens".into(), n.clone());
    }
    for k in ["temperature", "top_p"] {
        if let Some(v) = req.get(k) {
            body.insert(k.into(), v.clone());
        }
    }
    Value::Object(body)
}

fn push_input_item(item: &Value, messages: &mut Vec<Value>) {
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("message");
    match kind {
        "message" => {
            // Codex writes its harness prompt as `developer`, which Chat
            // Completions does not know; upstream 400s on the unknown role.
            let role = match item.get("role").and_then(Value::as_str) {
                Some("developer") | Some("system") => "system",
                Some("assistant") => "assistant",
                _ => "user",
            };
            let text = collect_text(item.get("content"));
            messages.push(json!({"role": role, "content": text}));
        }
        "function_call" => {
            let call_id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let call = json!({
                "id": call_id,
                "type": "function",
                "function": {
                    "name": item.get("name").and_then(Value::as_str).unwrap_or_default(),
                    "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or("{}"),
                }
            });
            // Parallel calls arrive as separate items but belong to one
            // assistant turn. Split across messages, the results no longer
            // follow their call and strict providers reject the whole turn
            // ("tool call result does not follow tool call").
            if let Some(prev) = messages.last_mut() {
                if prev["role"] == "assistant" && prev.get("tool_calls").is_some() {
                    prev["tool_calls"].as_array_mut().unwrap().push(call);
                    return;
                }
            }
            messages.push(json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [call],
            }));
        }
        "function_call_output" => {
            let out = item.get("output");
            let content = match out {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string()),
                None => String::new(),
            };
            messages.push(json!({
                "role": "tool",
                "tool_call_id": item.get("call_id").and_then(Value::as_str).unwrap_or_default(),
                "content": content,
            }));
        }
        // Reasoning items are replayed by the client but carry no chat
        // equivalent, and upstream would reject the unknown role.
        _ => {}
    }
}

fn collect_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn tool_label(t: &Value) -> String {
    let kind = t.get("type").and_then(Value::as_str).unwrap_or("?");
    match t.get("name").and_then(Value::as_str) {
        Some(n) => format!("{kind}:{n}"),
        None => kind.to_string(),
    }
}

/// Chat Completions has one tool shape: a flat function. Namespaced tool
/// groups (Codex's MCP bundles) and built-ins like `web_search` have no
/// equivalent, so they are dropped — the core coding tools are plain
/// functions and survive.
fn to_chat_tool(t: &Value) -> Option<Value> {
    // Responses puts the function inline; Chat nests it under `function`.
    match t.get("type").and_then(Value::as_str) {
        Some("function") | None => {
            let (name, desc, params) = if let Some(f) = t.get("function") {
                (f.get("name"), f.get("description"), f.get("parameters"))
            } else {
                (t.get("name"), t.get("description"), t.get("parameters"))
            };
            Some(json!({
                "type": "function",
                "function": {
                    "name": name.cloned().unwrap_or(Value::Null),
                    "description": desc.cloned().unwrap_or(json!("")),
                    "parameters": params.cloned().unwrap_or(json!({"type": "object", "properties": {}})),
                }
            }))
        }
        // freeform / local_shell / web_search have no Chat equivalent
        _ => None,
    }
}

fn to_chat_tool_choice(tc: &Value) -> Value {
    match tc {
        Value::String(_) => tc.clone(),
        Value::Object(o) => match o.get("name").and_then(Value::as_str) {
            Some(n) => json!({"type": "function", "function": {"name": n}}),
            None => json!("auto"),
        },
        _ => json!("auto"),
    }
}

// ------------------------------------------------- Chat -> Responses ------

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Piece {
    Text,
    Think,
}

/// Length of the longest suffix of `buf` that is a proper prefix of `tag`.
fn partial_tag_suffix(buf: &str, tag: &str) -> usize {
    let max = tag.len().saturating_sub(1).min(buf.len());
    for n in (1..=max).rev() {
        let start = buf.len() - n;
        if buf.is_char_boundary(start) && tag.starts_with(&buf[start..]) {
            return n;
        }
    }
    0
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    args: String,
}

/// Accumulates upstream chunks and emits Responses events as they arrive.
pub struct Turn {
    id: String,
    model: String,
    created: bool,
    text: String,
    reasoning: String,
    /// Keyed by the upstream chunk index, which is NOT guaranteed to start at
    /// zero — minimax numbers its calls from 1, and a positional Vec would
    /// invent an empty call at 0 that the next request rejects.
    calls: BTreeMap<u64, Call>,
    usage: Option<Value>,
    error: Option<String>,
    done: bool,
    in_think: bool,
    held: String,
}

impl Turn {
    pub fn new(model: &str) -> Self {
        Turn {
            id: String::new(),
            model: model.to_string(),
            created: false,
            text: String::new(),
            reasoning: String::new(),
            calls: BTreeMap::new(),
            usage: None,
            error: None,
            done: false,
            in_think: false,
            held: String::new(),
        }
    }

    fn msg_id(&self) -> String {
        format!("msg_{}", self.id)
    }

    /// Feed one upstream SSE line.
    pub fn feed<F>(&mut self, line: &str, emit: &mut F) -> std::io::Result<()>
    where
        F: FnMut(&Value) -> std::io::Result<()>,
    {
        let line = line.trim();
        let payload = match line.strip_prefix("data:") {
            Some(p) => p.trim(),
            None => return Ok(()),
        };
        if payload == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let chunk: Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        // A trailing `{"choices":[],"cost":"0"}` follows [DONE] on this gateway.
        if self.done {
            return Ok(());
        }
        if let Some(e) = chunk.get("error") {
            self.error = Some(
                e.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or(&e.to_string())
                    .to_string(),
            );
            return Ok(());
        }
        if self.id.is_empty() {
            if let Some(id) = chunk.get("id").and_then(Value::as_str) {
                self.id = id.to_string();
            }
        }
        if let Some(m) = chunk.get("model").and_then(Value::as_str) {
            self.model = m.to_string();
        }
        if let Some(u) = chunk.get("usage") {
            if !u.is_null() {
                self.usage = Some(u.clone());
            }
        }
        if !self.created {
            self.created = true;
            emit(&json!({
                "type": "response.created",
                "response": self.envelope("in_progress", false),
            }))?;
            emit(&json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": self.msg_id(), "type": "message",
                    "status": "in_progress", "role": "assistant", "content": [],
                },
            }))?;
        }

        let delta = match chunk.pointer("/choices/0/delta") {
            Some(d) => d,
            None => return Ok(()),
        };

        if let Some(t) = delta.get("content").and_then(Value::as_str) {
            if !t.is_empty() {
                for (kind, piece) in self.split_think(t) {
                    match kind {
                        Piece::Text => {
                            self.text.push_str(&piece);
                            emit(&json!({
                                "type": "response.output_text.delta",
                                "item_id": self.msg_id(),
                                "output_index": 0, "content_index": 0,
                                "delta": piece,
                            }))?;
                        }
                        Piece::Think => {
                            self.reasoning.push_str(&piece);
                            emit(&json!({
                                "type": "response.reasoning_summary_text.delta",
                                "item_id": format!("rs_{}", self.id),
                                "output_index": 0, "summary_index": 0,
                                "delta": piece,
                            }))?;
                        }
                    }
                }
            }
        }
        if let Some(r) = delta.get("reasoning_content").and_then(Value::as_str) {
            if !r.is_empty() {
                self.reasoning.push_str(r);
                emit(&json!({
                    "type": "response.reasoning_summary_text.delta",
                    "item_id": format!("rs_{}", self.id),
                    "output_index": 0, "summary_index": 0,
                    "delta": r,
                }))?;
            }
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc in tcs {
                let idx = tc.get("index").and_then(Value::as_u64).unwrap_or(0);
                let call = self.calls.entry(idx).or_default();
                // Only the first chunk of a call carries id and name; later
                // ones repeat `"id": ""`, which must not clobber it.
                if let Some(id) = tc.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        call.id = id.to_string();
                    }
                }
                if let Some(f) = tc.get("function") {
                    if let Some(n) = f.get("name").and_then(Value::as_str) {
                        if !n.is_empty() {
                            call.name = n.to_string();
                        }
                    }
                    if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                        call.args.push_str(a);
                    }
                }
            }
        }
        Ok(())
    }

    /// Some models (minimax) put their reasoning inline as `<think>...</think>`
    /// instead of in `reasoning_content`, and the harness would render the tags
    /// as the answer. Split the stream, holding back any partial tag that
    /// straddles a chunk boundary.
    fn split_think(&mut self, chunk: &str) -> Vec<(Piece, String)> {
        let mut out = Vec::new();
        let mut buf = std::mem::take(&mut self.held);
        buf.push_str(chunk);

        loop {
            let (tag, kind) = if self.in_think {
                (CLOSE, Piece::Think)
            } else {
                (OPEN, Piece::Text)
            };
            if let Some(i) = buf.find(tag) {
                if i > 0 {
                    out.push((kind, buf[..i].to_string()));
                }
                buf = buf[i + tag.len()..].to_string();
                self.in_think = !self.in_think;
                continue;
            }
            // No tag yet: keep back anything that could still become one.
            let keep = partial_tag_suffix(&buf, tag);
            let split = buf.len() - keep;
            if split > 0 {
                out.push((kind, buf[..split].to_string()));
            }
            self.held = buf[split..].to_string();
            break;
        }
        out
    }

    /// Close the turn: the item bodies the client actually reads.
    pub fn finish<F>(&mut self, emit: &mut F) -> std::io::Result<()>
    where
        F: FnMut(&Value) -> std::io::Result<()>,
    {
        if let Some(msg) = &self.error {
            return emit(&json!({
                "type": "response.failed",
                "response": {
                    "id": self.response_id(), "object": "response", "status": "failed",
                    "error": {"code": "upstream_error", "message": msg},
                },
            }));
        }
        if !self.created {
            emit(&json!({
                "type": "response.created",
                "response": self.envelope("in_progress", false),
            }))?;
        }
        // A held partial tag never completed — it was ordinary text.
        if !self.held.is_empty() {
            let held = std::mem::take(&mut self.held);
            if self.in_think {
                self.reasoning.push_str(&held);
            } else {
                self.text.push_str(&held);
                emit(&json!({
                    "type": "response.output_text.delta",
                    "item_id": self.msg_id(),
                    "output_index": 0, "content_index": 0,
                    "delta": held,
                }))?;
            }
        }
        for (i, item) in self.items().iter().enumerate() {
            emit(&json!({
                "type": "response.output_item.done",
                "output_index": i,
                "item": item,
            }))?;
        }
        emit(&json!({
            "type": "response.completed",
            "response": self.envelope("completed", true),
        }))
    }

    /// The upstream error this turn carried, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn response_id(&self) -> String {
        format!("resp_{}", self.id)
    }

    fn items(&self) -> Vec<Value> {
        let mut items = Vec::new();
        if !self.text.is_empty() {
            items.push(json!({
                "id": self.msg_id(), "type": "message", "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": self.text, "annotations": []}],
            }));
        }
        for (idx, c) in &self.calls {
            // A nameless entry is a stream artifact, not a call.
            if c.name.is_empty() {
                continue;
            }
            // Never emit an empty call_id: the harness echoes it back as the
            // tool result's key and upstream rejects the pairing.
            let call_id = if c.id.is_empty() {
                format!("call_{}_{idx}", self.id)
            } else {
                c.id.clone()
            };
            items.push(json!({
                "id": format!("fc_{}_{idx}", self.id),
                "type": "function_call", "status": "completed",
                "name": c.name,
                "arguments": if c.args.is_empty() { "{}" } else { &c.args },
                "call_id": call_id,
            }));
        }
        items
    }

    fn envelope(&self, status: &str, with_output: bool) -> Value {
        let mut r = json!({
            "id": self.response_id(),
            "object": "response",
            "status": status,
            "model": self.model,
            "output": if with_output { Value::Array(self.items()) } else { json!([]) },
        });
        if let Some(u) = &self.usage {
            let inp = u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
            let out = u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0);
            let cached = u
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let reasoning = u
                .pointer("/completion_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            r["usage"] = json!({
                "input_tokens": inp,
                "input_tokens_details": {"cached_tokens": cached},
                "output_tokens": out,
                "output_tokens_details": {"reasoning_tokens": reasoning},
                "total_tokens": inp + out,
            });
        }
        r
    }

    /// Whole-response body for a non-streaming request.
    pub fn response(&self, completed: bool) -> Value {
        self.envelope(if completed { "completed" } else { "incomplete" }, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_request() -> Value {
        json!({
            "model": "qwen3.8-max",
            "stream": true,
            "instructions": "You are Codex.",
            "input": [
                {"type": "message", "role": "developer",
                 "content": [{"type": "input_text", "text": "harness prompt"}]},
                {"type": "message", "role": "user",
                 "content": [{"type": "input_text", "text": "read marker.txt"}]},
                {"type": "function_call", "call_id": "call_1", "name": "exec_command",
                 "arguments": "{\"cmd\":\"cat marker.txt\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "hello"},
                {"type": "reasoning", "summary": []}
            ],
            "tools": [
                {"type": "function", "name": "exec_command", "description": "run",
                 "parameters": {"type": "object", "properties": {}}, "strict": false},
                {"type": "namespace", "name": "collaboration", "tools": []},
                {"type": "web_search"}
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "max_output_tokens": 2048
        })
    }

    #[test]
    fn developer_role_becomes_system() {
        // Chat Completions rejects `developer`, and the whole turn 400s.
        let chat = to_chat(&codex_request());
        let roles: Vec<&str> = chat["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, vec!["system", "system", "user", "assistant", "tool"]);
    }

    #[test]
    fn tool_calls_survive_the_round_trip() {
        let chat = to_chat(&codex_request());
        let msgs = chat["messages"].as_array().unwrap();
        let call = &msgs[3]["tool_calls"][0];
        assert_eq!(call["id"], "call_1");
        assert_eq!(call["function"]["name"], "exec_command");
        assert_eq!(msgs[4]["tool_call_id"], "call_1");
        assert_eq!(msgs[4]["content"], "hello");
    }

    #[test]
    fn parallel_calls_merge_into_one_assistant_turn() {
        let req = json!({
            "model": "minimax-m2.7",
            "input": [
                {"type": "message", "role": "user",
                 "content": [{"type": "input_text", "text": "go"}]},
                {"type": "function_call", "call_id": "a", "name": "f", "arguments": "{}"},
                {"type": "function_call", "call_id": "b", "name": "g", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "a", "output": "1"},
                {"type": "function_call_output", "call_id": "b", "output": "2"}
            ]
        });
        let chat = to_chat(&req);
        let msgs = chat["messages"].as_array().unwrap();
        let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, vec!["user", "assistant", "tool", "tool"]);
        assert_eq!(msgs[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(msgs[1]["tool_calls"][1]["id"], "b");
    }

    #[test]
    fn a_second_round_of_calls_starts_a_new_turn() {
        let req = json!({
            "model": "m",
            "input": [
                {"type": "function_call", "call_id": "a", "name": "f", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "a", "output": "1"},
                {"type": "function_call", "call_id": "b", "name": "f", "arguments": "{}"}
            ]
        });
        let chat = to_chat(&req);
        let roles: Vec<&str> = chat["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, vec!["assistant", "tool", "assistant"]);
    }

    #[test]
    fn only_plain_functions_reach_chat() {
        let chat = to_chat(&codex_request());
        let tools = chat["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "exec_command");
        assert_eq!(chat["max_tokens"], 2048);
        assert_eq!(chat["stream"], true);
    }

    fn drain(lines: &[&str]) -> Vec<Value> {
        let mut turn = Turn::new("qwen3.8-max");
        let mut events = Vec::new();
        {
            let mut sink = |e: &Value| {
                events.push(e.clone());
                Ok(())
            };
            for l in lines {
                turn.feed(l, &mut sink).unwrap();
            }
            turn.finish(&mut sink).unwrap();
        }
        events
    }

    #[test]
    fn text_streams_and_completes() {
        let events = drain(&[
            r#"data: {"id":"c1","model":"qwen3.8-max","choices":[{"delta":{"content":"he"}}]}"#,
            r#"data: {"id":"c1","choices":[{"delta":{"content":"llo"}}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
            "data: [DONE]",
            r#"data: {"choices":[],"cost":"0"}"#,
        ]);
        let types: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(types[0], "response.created");
        assert!(types.contains(&"response.output_text.delta"));
        assert_eq!(*types.last().unwrap(), "response.completed");

        let done = events.iter().find(|e| e["type"] == "response.output_item.done").unwrap();
        assert_eq!(done["item"]["content"][0]["text"], "hello");
        let completed = events.last().unwrap();
        assert_eq!(completed["response"]["usage"]["input_tokens"], 10);
        assert_eq!(completed["response"]["usage"]["total_tokens"], 12);
    }

    #[test]
    fn fragmented_tool_call_reassembles() {
        // The id arrives once and later chunks repeat `"id": ""` — clobbering
        // it would break the call_id the harness matches its output against.
        let events = drain(&[
            r#"data: {"id":"c2","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","function":{"name":"exec_command","arguments":""}}]}}]}"#,
            r#"data: {"id":"c2","choices":[{"delta":{"tool_calls":[{"index":0,"id":"","function":{"arguments":"{\"cmd\":"}}]}}]}"#,
            r#"data: {"id":"c2","choices":[{"delta":{"tool_calls":[{"index":0,"id":"","function":{"arguments":"\"ls\"}"}}]}}]}"#,
            "data: [DONE]",
        ]);
        let item = &events
            .iter()
            .find(|e| e["type"] == "response.output_item.done")
            .unwrap()["item"];
        assert_eq!(item["type"], "function_call");
        assert_eq!(item["call_id"], "call_x");
        assert_eq!(item["name"], "exec_command");
        assert_eq!(item["arguments"], r#"{"cmd":"ls"}"#);
    }

    #[test]
    fn tool_call_indices_need_not_start_at_zero() {
        // minimax-m2.7 numbers its calls from 1. A positional buffer invents
        // an empty call at 0, which the harness reports as an unsupported
        // call and upstream then rejects as an invalid tool call id.
        let events = drain(&[
            r#"data: {"id":"c7","choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_1","type":"function","function":{"name":"shell","arguments":""}}]}}]}"#,
            r#"data: {"id":"c7","choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"cmd\":\"ls\"}"}}]}}]}"#,
            "data: [DONE]",
        ]);
        let items: Vec<&Value> = events
            .iter()
            .filter(|e| e["type"] == "response.output_item.done")
            .map(|e| &e["item"])
            .collect();
        assert_eq!(items.len(), 1, "no phantom call");
        assert_eq!(items[0]["call_id"], "call_1");
        assert_eq!(items[0]["arguments"], r#"{"cmd":"ls"}"#);
    }

    #[test]
    fn a_call_with_no_id_still_gets_one() {
        let events = drain(&[
            r#"data: {"id":"c8","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"shell","arguments":"{}"}}]}}]}"#,
            "data: [DONE]",
        ]);
        let item = &events
            .iter()
            .find(|e| e["type"] == "response.output_item.done")
            .unwrap()["item"];
        assert_eq!(item["call_id"], "call_c8_0");
    }

    #[test]
    fn reasoning_is_surfaced_but_never_replayed() {
        let events = drain(&[
            r#"data: {"id":"c3","choices":[{"delta":{"reasoning_content":"thinking"}}]}"#,
            "data: [DONE]",
        ]);
        assert!(events
            .iter()
            .any(|e| e["type"] == "response.reasoning_summary_text.delta"));
        // No reasoning item: we cannot produce the encrypted content the
        // client would expect to send back on the next turn.
        assert!(events
            .iter()
            .all(|e| e["item"]["type"] != "reasoning"));
    }

    fn text_of(events: &[Value], kind: &str) -> String {
        events
            .iter()
            .filter(|e| e["type"] == kind)
            .filter_map(|e| e["delta"].as_str())
            .collect()
    }

    #[test]
    fn inline_think_tags_become_reasoning() {
        // minimax puts its reasoning in `content` rather than
        // `reasoning_content`; left alone, the harness renders the tags as
        // the answer.
        let events = drain(&[
            r#"data: {"id":"c4","choices":[{"delta":{"content":"<think>weighing it</think>hello"}}]}"#,
            "data: [DONE]",
        ]);
        assert_eq!(text_of(&events, "response.output_text.delta"), "hello");
        assert_eq!(
            text_of(&events, "response.reasoning_summary_text.delta"),
            "weighing it"
        );
        let item = &events
            .iter()
            .find(|e| e["type"] == "response.output_item.done")
            .unwrap()["item"];
        assert_eq!(item["content"][0]["text"], "hello");
    }

    #[test]
    fn a_think_tag_split_across_chunks_still_splits() {
        let events = drain(&[
            r#"data: {"id":"c5","choices":[{"delta":{"content":"<thi"}}]}"#,
            r#"data: {"id":"c5","choices":[{"delta":{"content":"nk>why</thi"}}]}"#,
            r#"data: {"id":"c5","choices":[{"delta":{"content":"nk>done"}}]}"#,
            "data: [DONE]",
        ]);
        assert_eq!(text_of(&events, "response.output_text.delta"), "done");
        assert_eq!(text_of(&events, "response.reasoning_summary_text.delta"), "why");
    }

    #[test]
    fn a_lone_angle_bracket_is_not_swallowed() {
        let events = drain(&[
            r#"data: {"id":"c6","choices":[{"delta":{"content":"if a < b"}}]}"#,
            "data: [DONE]",
        ]);
        assert_eq!(text_of(&events, "response.output_text.delta"), "if a < b");
    }

    #[test]
    fn partial_tag_suffix_finds_the_longest_hold() {
        assert_eq!(partial_tag_suffix("abc<thi", OPEN), 4);
        assert_eq!(partial_tag_suffix("abc<", OPEN), 1);
        assert_eq!(partial_tag_suffix("abc", OPEN), 0);
        assert_eq!(partial_tag_suffix("héllo<", OPEN), 1);
    }

    #[test]
    fn a_keep_alive_comment_still_counts_as_a_stream() {
        // minimax-m2.5 opens with `: keep-alive`; treating that as an error
        // body 502s every request before a token is read.
        let mut r = std::io::Cursor::new(": keep-alive\n\ndata: {\"id\":\"x\"}\n");
        let mut skipped = String::new();
        let first = peek_stream(&mut r, &mut skipped).unwrap();
        assert_eq!(first.unwrap().trim(), r#"data: {"id":"x"}"#);
    }

    #[test]
    fn a_json_error_body_is_not_a_stream() {
        let mut r = std::io::Cursor::new("{\"error\":{\"message\":\"nope\"}}");
        let mut skipped = String::new();
        assert!(peek_stream(&mut r, &mut skipped).unwrap().is_none());
        assert!(skipped.contains("nope"));
    }

    #[test]
    fn an_empty_body_is_not_a_stream() {
        let mut r = std::io::Cursor::new("");
        let mut skipped = String::new();
        assert!(peek_stream(&mut r, &mut skipped).unwrap().is_none());
    }

    #[test]
    fn upstream_error_becomes_response_failed() {
        let events = drain(&[
            r#"data: {"error":{"message":"model is overloaded"}}"#,
            "data: [DONE]",
        ]);
        let last = events.last().unwrap();
        assert_eq!(last["type"], "response.failed");
        assert_eq!(last["response"]["error"]["message"], "model is overloaded");
    }
}
