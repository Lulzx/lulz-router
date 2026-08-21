//! lulz — decouple coding-agent harnesses from model providers.
//!
//! `lulz launch claude` runs Claude Code against your OpenCode Go subscription.
//! Zero dependencies: shells out to `curl` for the model list and `exec()`s the
//! harness so signals, terminal state, cwd and exit codes behave natively.

mod proxy;

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const VERSION: &str = env!("CARGO_PKG_VERSION");

// OpenCode Go gateway. Claude Code appends `/v1/messages` to its base url,
// everything else wants the `/v1` root explicitly.
const GO_ROOT: &str = "https://opencode.ai/zen/go";
const GO_V1: &str = "https://opencode.ai/zen/go/v1";

const DEFAULT_CLAUDE_MODEL: &str = "qwen3.8-max";
const DEFAULT_CODEX_MODEL: &str = "gpt-5.6-luna";
const DEFAULT_SMALL_MODEL: &str = "deepseek-v4-flash";

/// What each model can actually drive, probed against the live gateway
/// (`lulz doctor` re-probes and caches the result over this baseline):
///   claude — Anthropic Messages *with tools*, the only shape a harness uses
///   codex  — OpenAI Responses
///   ctx    — real context window, else Claude Code assumes 200k
struct Caps {
    id: &'static str,
    ctx: u32,
    claude: bool,
    codex: bool,
}

const fn m(id: &'static str, ctx: u32, claude: bool, codex: bool) -> Caps {
    Caps { id, ctx, claude, codex }
}

const MODELS: &[Caps] = &[
    m("deepseek-v4-flash", 1000000, true, true),
    m("deepseek-v4-pro", 1000000, true, true),
    m("glm-5", 202752, false, false),
    m("glm-5.1", 202752, false, false),
    m("glm-5.2", 1000000, false, false),
    m("glm-5.3", 1000000, false, false),
    m("gpt-5.6-luna", 1050000, true, true),
    m("grok-4.5", 500000, false, true),
    m("hy3", 256000, false, false),
    m("hy3-preview", 256000, false, false),
    m("kimi-k2.5", 262144, false, false),
    m("kimi-k2.6", 262144, false, false),
    m("kimi-k2.7-code", 262144, false, false),
    m("kimi-k3", 1048576, true, false),
    m("mimo-v2-omni", 262144, false, false),
    m("mimo-v2-pro", 1048576, false, false),
    m("mimo-v2.5", 1000000, false, false),
    m("mimo-v2.5-pro", 1048576, false, false),
    m("minimax-m2.5", 204800, true, false),
    m("minimax-m2.7", 204800, true, false),
    m("minimax-m3", 1000000, true, false),
    m("muse-spark-1.2-contributor", 1048576, true, true),
    m("ox-alpha-free", 1000000, false, false),
    m("qwen3.5-plus", 262144, true, false),
    m("qwen3.6-plus", 1000000, true, false),
    m("qwen3.7-max", 1000000, true, false),
    m("qwen3.7-plus", 1000000, true, false),
    m("qwen3.8-max", 1000000, true, false),
];


const ALIASES: &[(&str, &str)] = &[
    ("qwen", "qwen3.8-max"),
    ("minimax", "minimax-m3"),
    ("glm", "glm-5.3"),
    ("kimi", "kimi-k3"),
    ("gpt", "gpt-5.6-luna"),
    ("luna", "gpt-5.6-luna"),
    ("grok", "grok-4.5"),
    ("deepseek", "deepseek-v4-pro"),
    ("mimo", "mimo-v2.5-pro"),
    ("hy", "hy3"),
];

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let head = args.first().map(String::as_str).unwrap_or("help");

    let r = match head {
        "launch" | "run" => cmd_launch(&args[1..]),
        "models" | "model" | "ls" => cmd_models(),
        "auth" | "key" => cmd_auth(&args[1..]),
        "doctor" | "probe" => cmd_doctor(),
        "default" | "defaults" => cmd_default(&args[1..]),
        "-V" | "--version" | "version" => {
            println!("lulz {VERSION}");
            Ok(())
        }
        "help" | "-h" | "--help" => {
            help();
            Ok(())
        }
        other => Err(format!("unknown command `{other}`\n\nrun `lulz help`")),
    };

    if let Err(e) = r {
        eprintln!("{} {e}", paint("error", "31;1"));
        std::process::exit(1);
    }
}

fn help() {
    println!(
        "\
{name} {VERSION}
run any coding-agent harness on your OpenCode Go subscription

{usage}
  lulz launch <harness> [-m <model>] [-- <harness args>...]
  lulz models
  lulz auth [--save]
  lulz doctor
  lulz default <harness> <model>

{harnesses}
  claude      Claude Code       (Anthropic Messages)
  codex       Codex CLI         (Responses / Chat Completions)
  opencode    OpenCode          (native)

{examples}
  lulz launch claude
  lulz launch claude -m minimax-m3
  lulz launch codex -m gpt-5.6-luna
  lulz launch codex -m qwen3.8-max     # bridged automatically
  lulz launch claude -- --resume
  lulz default claude qwen3.8-max

{flags}
  -m, --model <id>    model to run (alias ok: qwen, glm, kimi, gpt, grok, ...)
      --small <id>    background/fast model for Claude Code
  -t, --translate     force the Responses -> Chat Completions bridge
      --no-translate  refuse instead of bridging (codex talks to the gateway
                      directly, which only works for a few models)
      --print         print the resolved command and env, then exit
",
        name = paint("lulz", "35;1"),
        usage = paint("usage", "1"),
        harnesses = paint("harnesses", "1"),
        examples = paint("examples", "1"),
        flags = paint("flags", "1"),
    );
}

// ---------------------------------------------------------------- launch ---

struct LaunchOpts {
    model: Option<String>,
    small: Option<String>,
    print: bool,
    translate: bool,
    native: bool,
    rest: Vec<String>,
}

fn parse_launch(args: &[String]) -> Result<(String, LaunchOpts), String> {
    let mut it = args.iter();
    let harness = it
        .next()
        .ok_or("which harness? try `lulz launch claude`")?
        .to_string();

    let mut o =
        LaunchOpts {
            model: None,
            small: None,
            print: false,
            translate: false,
            native: false,
            rest: vec![],
        };
    let mut passthrough = false;

    while let Some(a) = it.next() {
        if passthrough {
            o.rest.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => passthrough = true,
            "-m" | "--model" => {
                o.model = Some(it.next().ok_or("-m needs a model id")?.clone());
            }
            "--small" => {
                o.small = Some(it.next().ok_or("--small needs a model id")?.clone());
            }
            "--print" | "--dry-run" => o.print = true,
            "-t" | "--translate" => o.translate = true,
            "--no-translate" | "--native" => o.native = true,
            _ => o.rest.push(a.clone()),
        }
    }
    Ok((harness, o))
}

fn cmd_launch(args: &[String]) -> Result<(), String> {
    let (harness, opts) = parse_launch(args)?;
    let key = find_key()?.value;
    let cfg = read_config();

    let pick = |dflt: &str| -> String {
        let raw = opts
            .model
            .clone()
            .or_else(|| cfg.get(&harness).cloned())
            .unwrap_or_else(|| dflt.to_string());
        resolve_alias(&raw)
    };

    let gate = |model: &str| -> Result<(), String> {
        if opts.translate || can_run(&harness, model) {
            return Ok(());
        }
        let alt = if harness == "codex" { "claude" } else { "codex" };
        let ok = best_for(&harness);
        let shown = ok.iter().take(5).copied().collect::<Vec<_>>().join(", ");
        let more = if ok.len() > 5 {
            format!(" (+{} more, see `lulz models`)", ok.len() - 5)
        } else {
            String::new()
        };
        let mut msg =
            format!("`{model}` can't drive {harness} through this gateway.\n  works on {harness}: {shown}{more}");
        if can_run(alt, model) {
            msg.push_str(&format!("\n  or keep the model: lulz launch {alt} -m {model}"));
        }
        if harness == "codex" {
            msg.push_str("\n  or drop --no-translate and let lulz bridge it");
        }
        Err(msg)
    };

    let mut translate = opts.translate;
    let (bin, model, argv, env): (&str, String, Vec<String>, Vec<(String, String)>) =
        match harness.as_str() {
            "claude" => {
                let model = pick(DEFAULT_CLAUDE_MODEL);
                gate(&model)?;
                let small = opts
                    .small
                    .clone()
                    .map(|s| resolve_alias(&s))
                    .unwrap_or_else(|| DEFAULT_SMALL_MODEL.to_string());
                // The gateway's Messages endpoint authenticates on `x-api-key`
                // only; ANTHROPIC_AUTH_TOKEN would send `Authorization: Bearer`
                // and come back 401.
                let mut env = vec![
                    ("ANTHROPIC_BASE_URL".into(), GO_ROOT.into()),
                    ("ANTHROPIC_API_KEY".into(), key.clone()),
                    ("ANTHROPIC_MODEL".into(), model.clone()),
                    ("ANTHROPIC_DEFAULT_OPUS_MODEL".into(), model.clone()),
                    ("ANTHROPIC_DEFAULT_SONNET_MODEL".into(), model.clone()),
                    ("ANTHROPIC_DEFAULT_HAIKU_MODEL".into(), small.clone()),
                    ("ANTHROPIC_SMALL_FAST_MODEL".into(), small),
                    ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into()),
                ];
                if let Some(ctx) = context_window(&model) {
                    env.push(("CLAUDE_CODE_MAX_CONTEXT_TOKENS".into(), ctx.to_string()));
                }
                ("claude", model, opts.rest.clone(), env)
            }
            "codex" => {
                let model = pick(DEFAULT_CODEX_MODEL);
                // The gateway serves /responses for a handful of models and
                // Chat Completions for all of them, so bridge by default
                // rather than refusing a model that plainly works.
                translate = opts.translate || (!opts.native && !can_run("codex", &model));
                if opts.native {
                    gate(&model)?;
                }
                let base = if translate {
                    let port = proxy::spawn(proxy::Upstream {
                        base: GO_V1.to_string(),
                        key: key.clone(),
                    })
                    .map_err(|e| format!("could not start the translator: {e}"))?;
                    format!("http://127.0.0.1:{port}/v1")
                } else {
                    GO_V1.to_string()
                };
                let mut argv = vec![
                    "-c".into(), "model_provider=\"opencodego\"".into(),
                    "-c".into(), format!(
                        "model_providers.opencodego.name=\"OpenCode Go{}\"",
                        if translate { " (lulz)" } else { "" }
                    ),
                    "-c".into(), format!("model_providers.opencodego.base_url=\"{base}\""),
                    "-c".into(), "model_providers.opencodego.env_key=\"OPENCODE_API_KEY\"".into(),
                    "-c".into(), "model_providers.opencodego.wire_api=\"responses\"".into(),
                    "-c".into(), format!("model=\"{model}\""),
                ];
                argv.extend(opts.rest.clone());
                let env = vec![("OPENCODE_API_KEY".into(), key.clone())];
                ("codex", model, argv, env)
            }
            "opencode" => {
                let model = pick(DEFAULT_CLAUDE_MODEL);
                let mut argv = vec!["--model".into(), format!("opencode-go/{model}")];
                argv.extend(opts.rest.clone());
                let env = vec![("OPENCODE_API_KEY".into(), key.clone())];
                ("opencode", model, argv, env)
            }
            other => {
                return Err(format!(
                    "unknown harness `{other}` — expected claude, codex or opencode"
                ))
            }
        };

    let path = which(bin).ok_or_else(|| format!("`{bin}` is not on your PATH"))?;

    if opts.print {
        for (k, v) in &env {
            println!("{k}={}", if k.contains("TOKEN") || k.contains("KEY") { mask(v) } else { v.clone() });
        }
        println!("{} {}", path.display(), argv.join(" "));
        return Ok(());
    }

    banner(&harness, &model, translate);

    let mut cmd = Command::new(&path);
    cmd.args(&argv);
    for (k, v) in env {
        cmd.env(k, v);
    }
    if harness == "claude" {
        // A stray token would out-rank ANTHROPIC_API_KEY.
        cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
    }
    if translate {
        // The translator runs inside this process, and exec() would wipe it
        // out — so hand the terminal to a child and mirror its exit code.
        let status = cmd
            .status()
            .map_err(|e| format!("failed to run {}: {e}", path.display()))?;
        std::process::exit(status.code().unwrap_or(1));
    }
    Err(format!("failed to exec {}: {}", path.display(), cmd.exec()))
}

fn banner(harness: &str, model: &str, translate: bool) {
    if !std::io::stdout().is_terminal() {
        return;
    }
    let label = match harness {
        "claude" => "Claude Code",
        "codex" => "Codex",
        _ => "OpenCode",
    };
    eprintln!();
    eprintln!("  {}", paint("lulz", "35;1"));
    eprintln!("  {}   {label}", paint("harness", "2"));
    eprintln!("  {}  OpenCode Go", paint("provider", "2"));
    eprintln!("  {}     {model}", paint("model", "2"));
    if translate {
        eprintln!("  {} responses -> chat completions", paint("bridge", "2"));
    }
    eprintln!();
}

// ---------------------------------------------------------------- models ---

fn cmd_models() -> Result<(), String> {
    let key = find_key()?.value;
    let body = curl(&format!("{GO_V1}/models"), &key)?;
    let ids = json_ids(&body);
    if ids.is_empty() {
        return Err(format!("could not read the model list:\n{}", body.trim()));
    }

    println!("\n{}\n", paint("OpenCode Go models", "1"));
    println!(
        "  {:<28} {:>9}  {:<8} {:<8} {}",
        paint("model", "2"),
        paint("context", "2"),
        paint("claude", "2"),
        paint("codex", "2"),
        paint("opencode", "2")
    );
    for id in &ids {
        let ctx = context_window(id)
            .map(|c| format!("{}k", c / 1000))
            .unwrap_or_else(|| "?".into());
        // Codex reaches every model — natively where the gateway serves
        // /responses, over the bridge everywhere else.
        let codex = if can_run("codex", id) {
            paint("yes", "32")
        } else {
            paint("bridge", "36")
        };
        println!(
            "  {id:<28} {ctx:>9}  {:<8} {:<8} {}",
            mark(can_run("claude", id)),
            codex,
            mark(true)
        );
    }
    println!(
        "\n  {} lulz launch claude -m {}\n  {} lulz doctor\n",
        paint("run:  ", "2"),
        DEFAULT_CLAUDE_MODEL,
        paint("check:", "2"),
    );
    Ok(())
}

fn caps(model: &str) -> Option<&'static Caps> {
    MODELS.iter().find(|c| c.id == model)
}

/// Baseline table, overlaid with whatever `lulz doctor` last measured.
/// Unknown models are assumed capable — the harness reports the truth.
fn can_run(harness: &str, model: &str) -> bool {
    if let Some(v) = probe_cache().get(&format!("{harness}:{model}")) {
        return v == "ok";
    }
    match caps(model) {
        Some(c) if harness == "codex" => c.codex,
        Some(c) => c.claude,
        None => true,
    }
}

fn context_window(model: &str) -> Option<u32> {
    caps(model).map(|c| c.ctx)
}

fn best_for(harness: &str) -> Vec<&'static str> {
    MODELS
        .iter()
        .filter(|c| can_run(harness, c.id))
        .map(|c| c.id)
        .collect()
}

fn mark(ok: bool) -> String {
    if ok {
        paint("yes", "32")
    } else {
        paint(" - ", "2")
    }
}

fn resolve_alias(raw: &str) -> String {
    ALIASES
        .iter()
        .find(|(a, _)| *a == raw)
        .map(|(_, m)| m.to_string())
        .unwrap_or_else(|| raw.to_string())
}

// ---------------------------------------------------------------- doctor ---

fn cache_path() -> PathBuf {
    home().join(".cache/lulz/caps")
}

/// `harness:model` -> "ok" | "no", as last measured by `lulz doctor`.
fn probe_cache() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let Ok(s) = fs::read_to_string(cache_path()) {
        for line in s.lines() {
            if let Some((k, v)) = line.trim().split_once('=') {
                m.insert(k.to_string(), v.to_string());
            }
        }
    }
    m
}

const TOOL_PROBE: &str = concat!(
    r#"{"model":"%M","max_tokens":48,"#,
    r#""tools":[{"name":"ls","description":"list a directory","input_schema":"#,
    r#"{"type":"object","properties":{"p":{"type":"string"}},"required":["p"]}}],"#,
    r#""messages":[{"role":"user","content":[{"type":"text","text":"list /tmp using the tool"}]}]}"#
);

/// Ask the gateway what it will actually accept, rather than trusting the
/// baseline table. Tools are the point: a model that 400s on a tool schema is
/// useless to a coding harness even though plain chat works.
fn cmd_doctor() -> Result<(), String> {
    let key = find_key()?.value;
    let ids = json_ids(&curl(&format!("{GO_V1}/models"), &key)?);
    if ids.is_empty() {
        return Err("could not read the model list".into());
    }

    println!("\n{} {}\n", paint("probing", "1"), paint(GO_V1, "2"));
    let mut out = String::new();
    for id in &ids {
        let claude = post_status(
            &format!("{GO_V1}/messages"),
            &["-H", &format!("x-api-key: {key}"), "-H", "anthropic-version: 2023-06-01"],
            &TOOL_PROBE.replace("%M", id),
        ) == 200;
        let codex = post_status(
            &format!("{GO_V1}/responses"),
            &["-H", &format!("Authorization: Bearer {key}")],
            &format!(r#"{{"model":"{id}","input":"hi","max_output_tokens":16}}"#),
        ) == 200;
        println!("  {id:<28} claude {:<8} codex {}", mark(claude), mark(codex));
        out.push_str(&format!(
            "claude:{id}={}\ncodex:{id}={}\n",
            if claude { "ok" } else { "no" },
            if codex { "ok" } else { "no" }
        ));
    }

    let p = cache_path();
    fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&p, out).map_err(|e| e.to_string())?;
    println!("\n  cached to {}\n", p.display());
    Ok(())
}

fn post_status(url: &str, headers: &[&str], body: &str) -> u32 {
    let mut c = Command::new("curl");
    c.args(["-sS", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "90"]);
    c.args(headers);
    c.args(["-H", "content-type: application/json", "-d", body, url]);
    c.output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0)
}

// ------------------------------------------------------------------ auth ---

struct Key {
    value: String,
    source: String,
}

/// env → macOS Keychain → the OpenCode CLI's own auth.json.
fn find_key() -> Result<Key, String> {
    if let Ok(v) = env::var("OPENCODE_API_KEY") {
        if !v.is_empty() {
            return Ok(Key { value: v, source: "OPENCODE_API_KEY".into() });
        }
    }
    if let Some(v) = keychain_get() {
        return Ok(Key { value: v, source: "macOS Keychain".into() });
    }
    let auth = home().join(".local/share/opencode/auth.json");
    if let Ok(s) = fs::read_to_string(&auth) {
        if let Some(v) = json_key_in_object(&s, "opencode-go", "key") {
            return Ok(Key { value: v, source: "opencode auth.json".into() });
        }
    }
    Err("no OpenCode Go key found.\n  run `opencode auth login`, or set OPENCODE_API_KEY,\n  then `lulz auth --save` to stash it in the Keychain".into())
}

fn cmd_auth(args: &[String]) -> Result<(), String> {
    let k = find_key()?;
    if args.iter().any(|a| a == "--save") {
        keychain_set(&k.value)?;
        println!("saved to macOS Keychain (service `lulz`, account `opencode-go`)");
        return Ok(());
    }
    println!("key     {}", mask(&k.value));
    println!("source  {}", k.source);
    println!("gateway {GO_V1}");
    Ok(())
}

fn keychain_get() -> Option<String> {
    let out = Command::new("security")
        .args(["find-generic-password", "-s", "lulz", "-a", "opencode-go", "-w"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

fn keychain_set(value: &str) -> Result<(), String> {
    let st = Command::new("security")
        .args(["add-generic-password", "-U", "-s", "lulz", "-a", "opencode-go", "-w", value])
        .status()
        .map_err(|e| format!("security: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err("keychain write failed".into())
    }
}

// --------------------------------------------------------------- defaults ---

fn config_path() -> PathBuf {
    home().join(".config/lulz/config")
}

fn read_config() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let Ok(s) = fs::read_to_string(config_path()) {
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                m.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    m
}

fn cmd_default(args: &[String]) -> Result<(), String> {
    let cfg = read_config();
    if args.is_empty() {
        if cfg.is_empty() {
            println!("no defaults set — try `lulz default claude qwen3.8-max`");
        }
        for (k, v) in &cfg {
            println!("{k:<10} {v}");
        }
        return Ok(());
    }
    let harness = args.first().unwrap();
    let model = args.get(1).ok_or("usage: lulz default <harness> <model>")?;
    let mut cfg = cfg;
    cfg.insert(harness.clone(), resolve_alias(model));

    let p = config_path();
    fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let body: String = cfg.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    fs::write(&p, body).map_err(|e| e.to_string())?;
    println!("{harness} → {}", cfg[harness]);
    Ok(())
}

// ----------------------------------------------------------------- utils ---

fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

fn curl(url: &str, key: &str) -> Result<String, String> {
    let out = Command::new("curl")
        .args(["-sS", "--max-time", "20", "-H", &format!("Authorization: Bearer {key}"), url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn mask(v: &str) -> String {
    if v.len() <= 10 {
        return "*".repeat(v.len());
    }
    format!("{}...{}", &v[..7], &v[v.len() - 4..])
}

fn paint(s: &str, code: &str) -> String {
    if std::io::stdout().is_terminal() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// Every `"id":"..."` in the payload, in order, deduped.
fn json_ids(body: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut rest = body;
    while let Some(i) = rest.find("\"id\"") {
        rest = &rest[i + 4..];
        let Some(c) = rest.find(':') else { break };
        let after = rest[c + 1..].trim_start();
        if let Some(v) = read_json_string(after) {
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out.sort();
    out
}

/// `obj`'s `field`, where `obj` is a top-level key of a small JSON object.
fn json_key_in_object(body: &str, obj: &str, field: &str) -> Option<String> {
    let start = body.find(&format!("\"{obj}\""))?;
    let scope = &body[start..];
    let f = scope.find(&format!("\"{field}\""))?;
    let after = &scope[f + field.len() + 2..];
    let c = after.find(':')?;
    read_json_string(after[c + 1..].trim_start())
}

/// Reads a JSON string literal at the head of `s` (handles \" escapes).
fn read_json_string(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.first() != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() => {
                out.push(b[i + 1] as char);
                i += 2;
            }
            b'"' => return Some(out),
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_opencode_auth_shape() {
        let s = r#"{"anthropic":{"type":"oauth"},"opencode-go":{"type":"api","key":"sk-abc"}}"#;
        assert_eq!(json_key_in_object(s, "opencode-go", "key").unwrap(), "sk-abc");
    }

    #[test]
    fn reads_the_models_list() {
        let s = r#"{"object":"list","data":[{"id":"glm-5"},{"id":"kimi-k3"},{"id":"glm-5"}]}"#;
        assert_eq!(json_ids(s), vec!["glm-5", "kimi-k3"]);
    }

    #[test]
    fn capability_gate_matches_the_probed_gateway() {
        assert!(can_run("claude", "qwen3.8-max"));
        assert!(!can_run("claude", "glm-5.3"));   // 400s on tool schemas
        assert!(!can_run("claude", "grok-4.5"));  // messages 401s, codex-only
        assert!(can_run("codex", "grok-4.5"));
        assert!(can_run("codex", "gpt-5.6-luna"));
        assert!(!can_run("codex", "kimi-k3"));    // no /responses
        assert!(can_run("claude", "some-new-model")); // unknown: let it try
    }

    #[test]
    fn best_for_lists_only_workable_models() {
        assert!(best_for("codex").contains(&"gpt-5.6-luna"));
        assert!(!best_for("codex").contains(&"kimi-k3"));
        assert!(best_for("claude").contains(&"minimax-m3"));
    }

    #[test]
    fn aliases_expand() {
        assert_eq!(resolve_alias("qwen"), "qwen3.8-max");
        assert_eq!(resolve_alias("glm-5.1"), "glm-5.1");
    }

    #[test]
    fn launch_args_split_at_dashdash() {
        let a: Vec<String> = ["claude", "-m", "kimi", "--", "--resume"]
            .iter().map(|s| s.to_string()).collect();
        let (h, o) = parse_launch(&a).unwrap();
        assert_eq!(h, "claude");
        assert_eq!(o.model.unwrap(), "kimi");
        assert_eq!(o.rest, vec!["--resume"]);
    }
}
