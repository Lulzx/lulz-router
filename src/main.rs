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
use std::time::{Duration, SystemTime};

const VERSION: &str = env!("CARGO_PKG_VERSION");

// OpenCode Go gateway. Claude Code appends `/v1/messages` to its base url,
// everything else wants the `/v1` root explicitly.
const GO_ROOT: &str = "https://opencode.ai/zen/go";
const GO_V1: &str = "https://opencode.ai/zen/go/v1";

const DEFAULT_CLAUDE_MODEL: &str = "minimax-m3";
const DEFAULT_CODEX_MODEL: &str = "gpt-5.6-luna";
const DEFAULT_SMALL_MODEL: &str = "deepseek-v4-flash";

/// What each model can actually drive, probed against the live gateway
/// (`lulz doctor` re-probes and caches the result over this baseline).
/// The *roster* is the gateway's own `/v1/models` — this table only carries
/// what that endpoint won't tell us:
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
    // The whole glm family 500s on /v1/messages — repeatedly, with and without
    // tools, while other models answer on the same endpoint in the same run.
    // Chat Completions serves it fine, so this is the gateway's Anthropic
    // shape, not the model. Context per models.dev, which /v1/models omits.
    m("glm-5.3-flash", 1000000, false, false),
    // Anthropic shape 500s the same way; /responses works, so codex-only.
    m("gpt-5.6-luna", 1050000, false, true),
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
        "models" | "model" | "ls" => cmd_models(&args[1..]),
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
  lulz models [--refresh]
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
  lulz default claude qwen3.8-max     # override the default

{flags}
  -m, --model <id>    model to run (alias ok: qwen, glm, kimi, gpt, grok, ...)
      --small <id>    background/fast model for Claude Code
  -t, --translate     force the Responses -> Chat Completions bridge
      --no-translate  refuse instead of bridging (codex talks to the gateway
                      directly, which only works for a few models)
      --print         print the resolved command and env, then exit
  -r, --refresh       (models) re-read /v1/models instead of the 12h cache
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

    // What the gateway actually serves, so a model it has never heard of is a
    // sentence from lulz rather than an opaque API error from the harness.
    let served = roster(&key, false);
    let pick = |dflt: &str| -> Result<String, String> {
        let raw = opts
            .model
            .clone()
            .or_else(|| cfg.get(&harness).cloned())
            .unwrap_or_else(|| dflt.to_string());
        ensure_served(resolve_alias(&raw), &served)
    };

    let gate = |model: &str| -> Result<(), String> {
        if opts.translate || can_run(&harness, model) {
            return Ok(());
        }
        let alt = if harness == "codex" { "claude" } else { "codex" };
        let ok = best_for(&harness);
        let shown = ok.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
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
                let model = pick(DEFAULT_CLAUDE_MODEL)?;
                gate(&model)?;
                let small = ensure_served(
                    opts.small
                        .clone()
                        .map(|s| resolve_alias(&s))
                        .unwrap_or_else(|| DEFAULT_SMALL_MODEL.to_string()),
                    &served,
                )?;
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
                let model = pick(DEFAULT_CODEX_MODEL)?;
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
                let model = pick(DEFAULT_CLAUDE_MODEL)?;
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
            println!("{k}={}", if is_secret(k) { mask(v) } else { v.clone() });
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

fn cmd_models(args: &[String]) -> Result<(), String> {
    let force = args.iter().any(|a| a == "--refresh" || a == "-r");
    let key = find_key()?.value;
    let ids = roster(&key, force);
    if ids.is_empty() {
        return Err(format!("could not read the model list from {GO_V1}/models"));
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
        "\n  {} lulz launch claude -m {}\n  {} lulz doctor\n  {} {}\n",
        paint("run:  ", "2"),
        DEFAULT_CLAUDE_MODEL,
        paint("check:", "2"),
        paint("list: ", "2"),
        roster_age_note(),
    );
    Ok(())
}

/// Where the printed roster came from, so a surprising list is explicable.
fn roster_age_note() -> String {
    match read_roster().map(|(_, age)| age.as_secs() / 60) {
        Some(0) => "fetched just now".into(),
        Some(m) if m < 60 => format!("cached {m}m ago — lulz models --refresh"),
        Some(m) => format!("cached {}h ago — lulz models --refresh", m / 60),
        None => "baseline table (gateway unreachable)".into(),
    }
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

fn best_for(harness: &str) -> Vec<String> {
    best_among(harness, &known_models())
}

fn best_among(harness: &str, ids: &[String]) -> Vec<String> {
    ids.iter().filter(|id| can_run(harness, id)).cloned().collect()
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

// ---------------------------------------------------------------- roster ---

/// How long a fetched model list stays fresh before `lulz` re-reads
/// `/v1/models`. The roster changes when OpenCode adds a model, not by the
/// minute, so half a day keeps launches instant without going stale.
const ROSTER_TTL: Duration = Duration::from_secs(12 * 3600);

fn roster_path() -> PathBuf {
    home().join(".cache/lulz/models")
}

/// Cached ids plus the age of the cache; `None` if it was never written.
fn read_roster() -> Option<(Vec<String>, Duration)> {
    let ids = parse_roster(&fs::read_to_string(roster_path()).ok()?);
    if ids.is_empty() {
        return None;
    }
    let age = fs::metadata(roster_path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or(ROSTER_TTL);
    Some((ids, age))
}

fn parse_roster(body: &str) -> Vec<String> {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

fn write_roster(ids: &[String]) {
    let p = roster_path();
    if fs::create_dir_all(p.parent().unwrap()).is_ok() {
        let _ = fs::write(&p, ids.join("\n") + "\n");
    }
}

/// The ids the gateway serves right now, cached at `~/.cache/lulz/models`.
///
/// A fetch failure falls back to the stale cache and then to the baseline
/// table, so a flaky network degrades the roster rather than blocking a
/// launch. `force` skips the cache — what `lulz models --refresh` and
/// `lulz doctor` want.
fn roster(key: &str, force: bool) -> Vec<String> {
    if !force {
        if let Some((ids, age)) = read_roster() {
            if age < ROSTER_TTL {
                return ids;
            }
        }
    }
    if let Ok(body) = curl(&format!("{GO_V1}/models"), key) {
        let ids = json_ids(&body);
        if !ids.is_empty() {
            write_roster(&ids);
            return ids;
        }
    }
    // Stale beats nothing; nothing beats a wrong answer. An empty roster is
    // "I don't know", and every caller treats that as "let it through".
    read_roster().map(|(ids, _)| ids).unwrap_or_default()
}

/// Best offline guess at the roster: the cache if there is one, else the
/// baseline table. Used where a network round-trip would be rude — printing
/// a suggestion after something already went wrong.
fn known_models() -> Vec<String> {
    read_roster()
        .map(|(ids, _)| ids)
        .unwrap_or_else(|| MODELS.iter().map(|c| c.id.to_string()).collect())
}

/// Reject a model the gateway doesn't serve *before* the harness starts and
/// reports it as a bare API error.
fn ensure_served(model: String, roster: &[String]) -> Result<String, String> {
    if roster.is_empty() || roster.iter().any(|id| *id == model) {
        return Ok(model);
    }
    // Everything up to the first digit — `glm-5.4-flash` suggests the glm family.
    let stem: String = model.chars().take_while(|c| !c.is_ascii_digit()).collect();
    let near: Vec<&str> = roster
        .iter()
        .filter(|id| stem.len() > 1 && id.starts_with(&stem))
        .map(String::as_str)
        .take(6)
        .collect();
    let mut msg = format!("`{model}` isn't served by this gateway.");
    if !near.is_empty() {
        msg.push_str(&format!("\n  close by: {}", near.join(", ")));
    }
    msg.push_str("\n  full list: lulz models");
    Err(msg)
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

/// What one probe proved. A capability cache may only record facts about the
/// *model*; an outage, a throttle or a bad key says nothing about it, and
/// writing "no" for those would gate a working model until the next probe.
enum Verdict {
    Ok,
    No,
    /// Every attempt came back with the same 5xx. That is either an outage or
    /// a route the gateway cannot serve for this model; only the rest of the
    /// run can tell which, so `settle` decides once the run is over.
    Down(u32),
    Unknown(u32),
}

/// Read the gateway's answer as a claim about the model, or refuse to.
///   200            — it works
///   400/404/422    — the model rejects this request shape: a real capability
///   401/403/429    — key, routing or throttle. Not about the model.
///   5xx, 0         — upstream is down or curl never got an answer.
fn verdict(status: u32) -> Verdict {
    match status {
        200 => Verdict::Ok,
        400 | 404 | 405 | 422 => Verdict::No,
        other => Verdict::Unknown(other),
    }
}

fn is_5xx(c: u32) -> bool {
    (500..600).contains(&c)
}

/// Probe once; on a 5xx, probe again. A single upstream error is noise, but
/// the *same* 5xx twice is a property of the route rather than a bad moment —
/// which is what lets a consistently broken model be gated at all.
fn probe(url: &str, headers: &[&str], body: &str) -> Verdict {
    match verdict(post_status(url, headers, body)) {
        Verdict::Unknown(c) if is_5xx(c) => match verdict(post_status(url, headers, body)) {
            Verdict::Unknown(c2) if c2 == c => Verdict::Down(c),
            second => second,
        },
        first => first,
    }
}

/// A repeated 5xx only says something about the *model* if the endpoint served
/// somebody else in the same run. If nothing got through, the gateway was down
/// and the run proves nothing — stay quiet and let the baseline stand.
fn settle(v: Verdict, endpoint_answered: bool) -> Verdict {
    match v {
        Verdict::Down(_) if endpoint_answered => Verdict::No,
        Verdict::Down(c) => Verdict::Unknown(c),
        other => other,
    }
}

fn show(v: &Verdict) -> String {
    match v {
        Verdict::Ok => paint("yes", "32"),
        Verdict::No => paint(" - ", "2"),
        Verdict::Down(c) => paint(&format!("{c}x2"), "31"),
        Verdict::Unknown(c) => paint(&format!("{c}?"), "33"),
    }
}

fn record(out: &mut String, harness: &str, id: &str, v: &Verdict) {
    match v {
        Verdict::Ok => out.push_str(&format!("{harness}:{id}=ok\n")),
        Verdict::No => out.push_str(&format!("{harness}:{id}=no\n")),
        // Deliberately unwritten — `can_run` then falls back to the baseline.
        // `Down` never reaches here: `settle` turns it into `No` or `Unknown`.
        Verdict::Down(_) | Verdict::Unknown(_) => {}
    }
}

/// Ask the gateway what it will actually accept, rather than trusting the
/// baseline table. Tools are the point: a model that 400s on a tool schema is
/// useless to a coding harness even though plain chat works.
fn cmd_doctor() -> Result<(), String> {
    let key = find_key()?.value;
    let ids = roster(&key, true);
    if ids.is_empty() {
        return Err("could not read the model list".into());
    }

    println!("\n{} {}\n", paint("probing", "1"), paint(GO_V1, "2"));
    // Whether each endpoint served *anybody* this run. Until that is known a
    // repeated 5xx cannot be read, so verdicts are settled after the loop.
    let mut claude_answered = false;
    let mut codex_answered = false;
    let mut results: Vec<(String, Verdict, Verdict)> = Vec::with_capacity(ids.len());
    for id in &ids {
        let claude = probe(
            &format!("{GO_V1}/messages"),
            &["-H", &format!("x-api-key: {key}"), "-H", "anthropic-version: 2023-06-01"],
            &TOOL_PROBE.replace("%M", id),
        );
        let codex = probe(
            &format!("{GO_V1}/responses"),
            &["-H", &format!("Authorization: Bearer {key}")],
            &format!(r#"{{"model":"{id}","input":"hi","max_output_tokens":16}}"#),
        );
        claude_answered |= matches!(claude, Verdict::Ok);
        codex_answered |= matches!(codex, Verdict::Ok);
        println!("  {id:<28} claude {:<12} codex {}", show(&claude), show(&codex));
        results.push((id.clone(), claude, codex));
    }

    let mut out = String::new();
    let mut unknown = 0usize;
    let mut auth_failures = 0usize;
    let mut gated: Vec<String> = Vec::new();
    for (id, claude, codex) in results {
        for (harness, v, answered) in
            [("claude", claude, claude_answered), ("codex", codex, codex_answered)]
        {
            let was_down = matches!(v, Verdict::Down(_));
            let v = settle(v, answered);
            if was_down && matches!(v, Verdict::No) {
                gated.push(format!("{harness}:{id}"));
            }
            if let Verdict::Unknown(c) = v {
                unknown += 1;
                if matches!(c, 401 | 403) {
                    auth_failures += 1;
                }
            }
            record(&mut out, harness, &id, &v);
        }
    }

    let p = cache_path();
    fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&p, out).map_err(|e| e.to_string())?;
    println!("\n  cached to {}", p.display());
    if !gated.is_empty() {
        println!(
            "  {} {} route(s) failed twice while the endpoint was serving others,\n  so they are now cached as unsupported: {}",
            paint("gated", "31"),
            gated.len(),
            gated.join(", "),
        );
    }
    if unknown > 0 {
        println!(
            "  {} {unknown} probe(s) were inconclusive — shown with their status code\n  and left uncached, so the baseline still applies. Re-run `lulz doctor` later.",
            paint("note", "33"),
        );
    }
    if auth_failures > 0 {
        println!(
            "  {} the gateway rejected the key on {auth_failures} probe(s). If that persists,\n  re-run `opencode auth login`, then `lulz auth --save`.",
            paint("auth", "31"),
        );
    }
    println!();
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

/// Credentials, not merely anything with TOKEN in the name —
/// CLAUDE_CODE_MAX_CONTEXT_TOKENS is a number worth reading in `--print`.
fn is_secret(k: &str) -> bool {
    k.ends_with("API_KEY") || k.ends_with("AUTH_TOKEN")
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
        // Both 500 on /v1/messages however often you ask; luna still has
        // /responses, so it stays a codex model rather than no model at all.
        assert!(!can_run("claude", "glm-5.3-flash"));
        assert!(!can_run("claude", "gpt-5.6-luna"));
    }

    /// The bug this table shipped with: the default model was marked
    /// claude-capable on a guess, and every `lulz launch claude` walked into a
    /// route the gateway will not serve. A default must be a verified one.
    #[test]
    fn the_defaults_can_drive_the_harness_they_default_for() {
        assert!(caps(DEFAULT_CLAUDE_MODEL).is_some_and(|c| c.claude));
        assert!(caps(DEFAULT_CODEX_MODEL).is_some_and(|c| c.codex));
        // Claude Code drives the small model over Messages too.
        assert!(caps(DEFAULT_SMALL_MODEL).is_some_and(|c| c.claude));
    }

    #[test]
    fn a_repeated_5xx_gates_only_when_the_endpoint_was_alive() {
        // Others got served, so the gateway was up and this route is the fault.
        assert!(matches!(settle(Verdict::Down(500), true), Verdict::No));
        // Nothing got through all run: an outage proves nothing about a model.
        assert!(matches!(settle(Verdict::Down(500), false), Verdict::Unknown(500)));
        // Everything else settles to itself.
        assert!(matches!(settle(Verdict::Ok, false), Verdict::Ok));
        assert!(matches!(settle(Verdict::Unknown(429), true), Verdict::Unknown(429)));
    }

    #[test]
    fn a_settled_outage_is_never_cached() {
        let mut out = String::new();
        record(&mut out, "claude", "up", &settle(Verdict::Down(500), true));
        record(&mut out, "claude", "down", &settle(Verdict::Down(503), false));
        assert_eq!(out, "claude:up=no\n");
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn best_for_lists_only_workable_models() {
        let all = ids(&["gpt-5.6-luna", "kimi-k3", "minimax-m3"]);
        assert_eq!(best_among("codex", &all), ids(&["gpt-5.6-luna"]));
        assert!(best_among("claude", &all).contains(&"minimax-m3".to_string()));
    }

    #[test]
    fn roster_cache_round_trips() {
        assert_eq!(
            parse_roster("# written by lulz\nglm-5.3\n\n  kimi-k3  \n"),
            ids(&["glm-5.3", "kimi-k3"])
        );
        assert!(parse_roster("").is_empty());
    }

    #[test]
    fn unserved_models_are_caught_before_launch() {
        let all = ids(&["glm-5.3", "glm-5.3-flash", "qwen3.8-max"]);
        assert_eq!(ensure_served("glm-5.3".into(), &all).unwrap(), "glm-5.3");
        let e = ensure_served("glm-9-turbo".into(), &all).unwrap_err();
        assert!(e.contains("isn't served"));
        assert!(e.contains("glm-5.3-flash"), "should suggest the family: {e}");
        // An empty roster means the gateway was unreachable — never a gate.
        assert!(ensure_served("anything".into(), &[]).is_ok());
    }

    #[test]
    fn only_the_model_itself_lands_in_the_cache() {
        let mut out = String::new();
        record(&mut out, "claude", "a", &verdict(200));
        record(&mut out, "claude", "b", &verdict(400));   // rejects tool schemas
        record(&mut out, "claude", "c", &verdict(401));   // key/routing, not the model
        record(&mut out, "claude", "d", &verdict(429));   // throttled
        record(&mut out, "claude", "e", &verdict(500));   // upstream down
        record(&mut out, "claude", "f", &verdict(0));     // curl never answered
        assert_eq!(out, "claude:a=ok\nclaude:b=no\n");
    }

    #[test]
    fn only_credentials_are_masked() {
        assert!(is_secret("ANTHROPIC_API_KEY"));
        assert!(is_secret("OPENCODE_API_KEY"));
        assert!(is_secret("ANTHROPIC_AUTH_TOKEN"));
        assert!(!is_secret("CLAUDE_CODE_MAX_CONTEXT_TOKENS"));
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
