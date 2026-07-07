// SPDX-License-Identifier: GPL-3.0-or-later
//! intercom — the station's two-way voice.
//!
//! `say` speaks through piper (TTS), `hear` listens through whisper.cpp (STT);
//! both are one-shot CPU subprocesses, so the mouth can speak while the GPU
//! paints — no VRAM-law conflict. Speaks the Station Convention: envelope
//! {status, tool, version, ts} on every one-shot, errors name the fix,
//! exit 0/2/3/4, isatty auto-switch, `--json` forces, `schema` self-describes.
//!
//! Engines and models are DATA: ~/Nexus/💻HomePC/🧰LocalModels/voice-models.json
//! (env INTERCOM_REGISTRY overrides). Artifacts carry sidecars (<out>.wav.json).

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TOOL: &str = "intercom";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const EXIT_OK: i32 = 0;
const EXIT_UNAVAILABLE: i32 = 2;
const EXIT_BAD_ARGS: i32 = 3;
const EXIT_RUNTIME: i32 = 4;

const HELP: &str = "\
intercom 0.1.0 — the station's two-way voice (local TTS + STT, one convention)

usage:
  intercom say TEXT|- [--voice ALIAS] [--out FILE.wav] [--play] [--json]
      speak through piper; '-' reads stdin; writes wav + .json sidecar
  intercom hear FILE.wav [--ear ALIAS] [--json]
      transcribe through whisper.cpp (any rate/channels; ffmpeg resamples)
  intercom hear --mic SECONDS [--ear ALIAS] [--json]
      record the default mic (pw-record), then transcribe
  intercom voices [--json]     the installed voices (registry + disk truth)
  intercom ears [--json]       the installed transcription models
  intercom status [--json]     engines · models · players, one envelope
  intercom schema              the machine-readable contract

exit codes: 0 ok · 2 unavailable (engine/model missing; fix names the cure)
            3 bad arguments · 4 runtime failure
registry: ~/Nexus/💻HomePC/🧰LocalModels/voice-models.json (INTERCOM_REGISTRY overrides)
the law: ~/Dev/ClaudeWorkspace/AGENT-CLI-STANDARD.md";

// ── plumbing ────────────────────────────────────────────────────────────────

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}
fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") { home().join(rest) } else { PathBuf::from(p) }
}
fn now_ts() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}
fn stdout_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

fn envelope(status: &str, payload: Value) -> Value {
    let mut m = Map::new();
    m.insert("status".into(), json!(status));
    m.insert("tool".into(), json!(TOOL));
    m.insert("version".into(), json!(VERSION));
    m.insert("ts".into(), json!(now_ts()));
    if let Value::Object(p) = payload {
        for (k, v) in p {
            m.insert(k, v);
        }
    }
    Value::Object(m)
}

fn emit(v: Value, code: i32) -> ! {
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()));
    std::process::exit(code);
}

fn fail(json: bool, code: i32, error: &str, fix: &str) -> ! {
    if json {
        emit(envelope("error", json!({ "error": error, "fix": fix })), code);
    }
    println!("intercom: {error}");
    if !fix.is_empty() {
        println!("  fix: {fix}");
    }
    std::process::exit(code);
}

struct Ran {
    exit: Option<i32>,
    stdout: String,
    stderr: String,
    elapsed_ms: u64,
}

fn run(argv: &[String], timeout_ms: u64, stdin_text: Option<&str>) -> Result<Ran, String> {
    let started = Instant::now();
    let mut cmd = Command::new(expand_home(&argv[0]));
    cmd.args(&argv[1..])
        .stdin(if stdin_text.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", argv[0]))?;
    if let (Some(text), Some(mut sin)) = (stdin_text, child.stdin.take()) {
        use std::io::Write;
        let _ = sin.write_all(text.as_bytes());
    }
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let exit = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(_) => break None,
        }
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut stdout);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut stderr);
    }
    Ok(Ran { exit, stdout, stderr, elapsed_ms: started.elapsed().as_millis() as u64 })
}

// ── the registry (config is data) ───────────────────────────────────────────

#[derive(Deserialize)]
struct Registry {
    engines: Engines,
    dir: String,
    #[serde(default)]
    voices: BTreeMap<String, ModelEntry>,
    #[serde(default)]
    ears: BTreeMap<String, ModelEntry>,
}

#[derive(Deserialize)]
struct Engines {
    tts: Engine,
    stt: Engine,
}

#[derive(Deserialize)]
struct Engine {
    name: String,
    bin: String,
    #[serde(default)]
    espeak_data: Option<String>,
}

#[derive(Deserialize)]
struct ModelEntry {
    file: String,
    #[serde(default)]
    config: Option<String>,
    #[serde(default)]
    default: bool,
    #[serde(default)]
    register: Option<String>,
}

fn registry_path() -> PathBuf {
    std::env::var_os("INTERCOM_REGISTRY")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("Nexus/💻HomePC/🧰LocalModels/voice-models.json"))
}

fn load_registry(json: bool) -> Registry {
    let path = registry_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => fail(
            json,
            EXIT_UNAVAILABLE,
            &format!("voice registry unreadable at {}: {e}", path.display()),
            "restore voice-models.json (a reference copy ships in the intercom repo README) or set INTERCOM_REGISTRY",
        ),
    };
    match serde_json::from_str(&text) {
        Ok(r) => r,
        Err(e) => fail(
            json,
            EXIT_RUNTIME,
            &format!("voice registry invalid: {e}"),
            &format!("fix the JSON at {}", path.display()),
        ),
    }
}

impl Registry {
    fn pick<'a>(
        &'a self,
        table: &'a BTreeMap<String, ModelEntry>,
        want: Option<&str>,
        kind: &str,
        json: bool,
    ) -> (&'a str, &'a ModelEntry) {
        if let Some(alias) = want {
            match table.get_key_value(alias) {
                Some((k, v)) => (k.as_str(), v),
                None => {
                    let known: Vec<&str> = table.keys().map(|s| s.as_str()).collect();
                    fail(
                        json,
                        EXIT_BAD_ARGS,
                        &format!("no {kind} named `{alias}`"),
                        &format!("one of: {} — or `intercom {kind}s --json`", known.join(", ")),
                    )
                }
            }
        } else {
            table
                .iter()
                .find(|(_, v)| v.default)
                .or_else(|| table.iter().next())
                .map(|(k, v)| (k.as_str(), v))
                .unwrap_or_else(|| {
                    fail(
                        json,
                        EXIT_UNAVAILABLE,
                        &format!("no {kind}s installed"),
                        &format!("add one to voice-models.json §{kind}s and place the file in models-voice/"),
                    )
                })
        }
    }
    fn model_file(&self, entry: &ModelEntry) -> PathBuf {
        expand_home(&self.dir).join(&entry.file)
    }
}

// ── wav helpers ─────────────────────────────────────────────────────────────

/// Read (sample_rate, channels, bits, data_bytes) from a RIFF/WAVE header —
/// enough truth for duration and the resample decision, no dependency.
fn wav_info(path: &Path) -> Option<(u32, u16, u16, u32)> {
    let bytes = {
        let mut f = std::fs::File::open(path).ok()?;
        let mut b = vec![0u8; 512.min(std::fs::metadata(path).ok()?.len() as usize)];
        f.read_exact(&mut b).ok()?;
        b
    };
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    // walk chunks to find fmt + data (some writers pad extra chunks)
    let mut pos = 12;
    let mut fmt: Option<(u32, u16, u16)> = None;
    let mut data_len: Option<u32> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let sz = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
        if id == b"fmt " && pos + 8 + 16 <= bytes.len() {
            let ch = u16::from_le_bytes(bytes[pos + 10..pos + 12].try_into().ok()?);
            let rate = u32::from_le_bytes(bytes[pos + 12..pos + 16].try_into().ok()?);
            let bits = u16::from_le_bytes(bytes[pos + 22..pos + 24].try_into().ok()?);
            fmt = Some((rate, ch, bits));
        }
        if id == b"data" {
            data_len = Some(sz as u32);
            break;
        }
        pos += 8 + sz + (sz & 1);
    }
    match (fmt, data_len) {
        (Some((r, c, b)), Some(d)) => Some((r, c, b, d)),
        (Some((r, c, b)), None) => {
            // data chunk beyond our 512-byte peek: derive from file size
            let total = std::fs::metadata(path).ok()?.len() as u32;
            Some((r, c, b, total.saturating_sub(44)))
        }
        _ => None,
    }
}

fn wav_seconds(path: &Path) -> Option<f64> {
    let (rate, ch, bits, data) = wav_info(path)?;
    if rate == 0 || ch == 0 || bits == 0 {
        return None;
    }
    Some(data as f64 / (rate as f64 * ch as f64 * (bits as f64 / 8.0)))
}

// ── verbs ───────────────────────────────────────────────────────────────────

fn cmd_status(reg: &Registry, json: bool) -> ! {
    let tts_bin = expand_home(&reg.engines.tts.bin);
    let stt_bin = expand_home(&reg.engines.stt.bin);
    let voice_ok = reg.voices.iter().filter(|(_, v)| reg.model_file(v).is_file()).count();
    let ear_ok = reg.ears.iter().filter(|(_, v)| reg.model_file(v).is_file()).count();
    let tts_ready = tts_bin.is_file() && voice_ok > 0;
    let stt_ready = stt_bin.is_file() && ear_ok > 0;
    let payload = json!({
        "tts_ready": tts_ready,
        "stt_ready": stt_ready,
        "tts": { "engine": reg.engines.tts.name, "bin": reg.engines.tts.bin,
                 "bin_present": tts_bin.is_file(), "voices_installed": voice_ok },
        "stt": { "engine": reg.engines.stt.name, "bin": reg.engines.stt.bin,
                 "bin_present": stt_bin.is_file(), "ears_installed": ear_ok },
        "play": { "pw_play": which("pw-play"), "pw_record": which("pw-record"), "ffmpeg": which("ffmpeg") },
        "registry": registry_path().to_string_lossy(),
    });
    if json {
        emit(envelope("ok", payload), EXIT_OK);
    }
    println!("intercom — the station's two-way voice");
    println!("  mouth  {} {}", if tts_ready { "●" } else { "▢" }, reg.engines.tts.name);
    println!("  ears   {} {}", if stt_ready { "●" } else { "▢" }, reg.engines.stt.name);
    println!("  voices {voice_ok} installed · ears {ear_ok} installed");
    std::process::exit(if tts_ready && stt_ready { EXIT_OK } else { EXIT_UNAVAILABLE });
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

fn list_models(reg: &Registry, table: &BTreeMap<String, ModelEntry>, kind: &str, json: bool) -> ! {
    let rows: Vec<Value> = table
        .iter()
        .map(|(alias, m)| {
            json!({
                "alias": alias,
                "file": m.file,
                "present": reg.model_file(m).is_file(),
                "default": m.default,
                "register": m.register,
            })
        })
        .collect();
    if json {
        emit(envelope("ok", json!({ kind: rows })), EXIT_OK);
    }
    for r in &rows {
        println!(
            " {} {:<12} {}  {}",
            if r["present"].as_bool().unwrap_or(false) { "●" } else { "▢" },
            r["alias"].as_str().unwrap_or(""),
            if r["default"].as_bool().unwrap_or(false) { "(default)" } else { "         " },
            r["register"].as_str().unwrap_or("")
        );
    }
    std::process::exit(EXIT_OK);
}

fn cmd_say(reg: &Registry, args: &[String], json: bool) -> ! {
    let mut text: Option<String> = None;
    let mut voice: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut play = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--voice" => voice = it.next().cloned(),
            "--out" => out = it.next().map(|s| expand_home(s)),
            "--play" => play = true,
            "-" => {
                let mut buf = String::new();
                let _ = std::io::stdin().read_to_string(&mut buf);
                text = Some(buf);
            }
            other if !other.starts_with("--") => {
                text = Some(match text {
                    None => other.to_string(),
                    Some(prev) => format!("{prev} {other}"),
                })
            }
            other => fail(json, EXIT_BAD_ARGS, &format!("unknown flag {other}"), "intercom --help"),
        }
    }
    let text = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| {
        fail(json, EXIT_BAD_ARGS, "say needs text", "intercom say \"hello\" — or pipe: … | intercom say -")
    });

    let (alias, entry) = reg.pick(&reg.voices, voice.as_deref(), "voice", json);
    let model = reg.model_file(entry);
    let bin = expand_home(&reg.engines.tts.bin);
    if !bin.is_file() {
        fail(json, EXIT_UNAVAILABLE, &format!("piper binary missing at {}", bin.display()),
             "re-fetch the release into 📁Projects/piper-current (github.com/rhasspy/piper)");
    }
    if !model.is_file() {
        fail(json, EXIT_UNAVAILABLE, &format!("voice `{alias}` file missing: {}", model.display()),
             "download it into models-voice/ (see voice-models.json ladder) or pick another: intercom voices");
    }

    let out = out.unwrap_or_else(|| {
        let dir = home().join(".local/share/intercom/said");
        let _ = std::fs::create_dir_all(&dir);
        dir.join(format!("say-{}.wav", chrono::Local::now().format("%Y%m%d-%H%M%S")))
    });
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    let mut argv: Vec<String> = vec![
        bin.to_string_lossy().into_owned(),
        "--model".into(),
        model.to_string_lossy().into_owned(),
        "--output_file".into(),
        out.to_string_lossy().into_owned(),
    ];
    if let Some(cfg) = &entry.config {
        argv.push("--config".into());
        argv.push(expand_home(&reg.dir).join(cfg).to_string_lossy().into_owned());
    }
    if let Some(ed) = &reg.engines.tts.espeak_data {
        argv.push("--espeak_data".into());
        argv.push(expand_home(ed).to_string_lossy().into_owned());
    }

    let ran = match run(&argv, 120_000, Some(&text)) {
        Ok(r) => r,
        Err(e) => fail(json, EXIT_RUNTIME, &format!("could not start piper: {e}"), "check the bin path in voice-models.json"),
    };
    if ran.exit != Some(0) {
        let tail: String = ran.stderr.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" · ");
        fail(json, EXIT_RUNTIME, &format!("piper failed (exit {:?}): {tail}", ran.exit),
             "run the same command by hand for the full stderr");
    }
    let seconds = wav_seconds(&out).unwrap_or(0.0);

    // the sidecar law: no orphan outputs
    let sidecar = out.with_extension("wav.json");
    let side = json!({
        "tool": TOOL, "version": VERSION, "ts": now_ts(),
        "text": text, "voice": alias, "model": entry.file,
        "seconds": (seconds * 100.0).round() / 100.0, "elapsed_ms": ran.elapsed_ms,
    });
    let _ = std::fs::write(&sidecar, serde_json::to_string_pretty(&side).unwrap_or_default());

    let mut played = false;
    if play {
        if which("pw-play") {
            played = run(&["pw-play".into(), out.to_string_lossy().into_owned()], 120_000, None)
                .map(|r| r.exit == Some(0))
                .unwrap_or(false);
        } else if json {
            // stay honest in the envelope; played stays false
        } else {
            println!("  (no pw-play on PATH — skipped playback)");
        }
    }

    let payload = json!({
        "wav": out.to_string_lossy(),
        "sidecar": sidecar.to_string_lossy(),
        "voice": alias,
        "seconds": (seconds * 100.0).round() / 100.0,
        "elapsed_ms": ran.elapsed_ms,
        "played": played,
    });
    if json {
        emit(envelope("ok", payload), EXIT_OK);
    }
    println!(
        "said {:.1}s as `{alias}` in {}ms → {}{}",
        seconds,
        ran.elapsed_ms,
        out.display(),
        if played { " (played)" } else { "" }
    );
    std::process::exit(EXIT_OK);
}

fn cmd_hear(reg: &Registry, args: &[String], json: bool) -> ! {
    let mut input: Option<PathBuf> = None;
    let mut ear: Option<String> = None;
    let mut mic_seconds: Option<u32> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--ear" => ear = it.next().cloned(),
            "--mic" => {
                mic_seconds = it.next().and_then(|s| s.parse().ok());
                if mic_seconds.is_none() {
                    fail(json, EXIT_BAD_ARGS, "--mic needs whole seconds", "intercom hear --mic 6");
                }
            }
            other if !other.starts_with("--") => input = Some(expand_home(other)),
            other => fail(json, EXIT_BAD_ARGS, &format!("unknown flag {other}"), "intercom --help"),
        }
    }

    let (alias, entry) = reg.pick(&reg.ears, ear.as_deref(), "ear", json);
    let model = reg.model_file(entry);
    let bin = expand_home(&reg.engines.stt.bin);
    if !bin.is_file() {
        fail(json, EXIT_UNAVAILABLE, &format!("whisper-cli missing at {}", bin.display()),
             "build it: cmake -B build && cmake --build build --target whisper-cli (in 📁Projects/whisper.cpp-current)");
    }
    if !model.is_file() {
        fail(json, EXIT_UNAVAILABLE, &format!("ear `{alias}` file missing: {}", model.display()),
             "download ggml model into models-voice/ (huggingface.co/ggerganov/whisper.cpp) or: intercom ears");
    }

    // the mic path records first, then falls through to transcription
    let scratch = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("intercom");
    let _ = std::fs::create_dir_all(&scratch);
    let mut recorded = false;
    let input = if let Some(secs) = mic_seconds {
        if !which("pw-record") {
            fail(json, EXIT_UNAVAILABLE, "pw-record not on PATH", "install pipewire-utils (Mint: pipewire-bin)");
        }
        let rec = scratch.join(format!("mic-{}.wav", chrono::Local::now().format("%H%M%S")));
        let argv: Vec<String> = ["timeout", &secs.to_string(), "pw-record", "--rate", "16000", "--channels", "1", rec.to_string_lossy().as_ref()]
            .iter().map(|s| s.to_string()).collect();
        let ran = run(&argv, (secs as u64 + 5) * 1000, None);
        match ran {
            Ok(_) if rec.is_file() && std::fs::metadata(&rec).map(|m| m.len() > 44).unwrap_or(false) => {}
            _ => fail(json, EXIT_RUNTIME, "mic capture produced nothing", "is a microphone connected? pw-record --list-targets"),
        }
        recorded = true;
        rec
    } else {
        input.unwrap_or_else(|| {
            fail(json, EXIT_BAD_ARGS, "hear needs a wav file or --mic N", "intercom hear voice.wav · intercom hear --mic 6")
        })
    };
    if !input.is_file() {
        fail(json, EXIT_BAD_ARGS, &format!("no such file: {}", input.display()), "point hear at a wav (or use --mic N)");
    }

    // whisper.cpp wants 16 kHz mono s16 — resample deterministically when needed
    let audio_seconds = wav_seconds(&input).unwrap_or(0.0);
    let needs_resample = wav_info(&input).map(|(r, c, b, _)| r != 16_000 || c != 1 || b != 16).unwrap_or(true);
    let wav16 = if needs_resample {
        if !which("ffmpeg") {
            fail(json, EXIT_UNAVAILABLE, "input isn't 16 kHz mono s16 and ffmpeg is missing", "install ffmpeg (it ships with phosphor's deps) or provide 16 kHz mono wav");
        }
        let conv = scratch.join("hear-16k.wav");
        let argv: Vec<String> = ["ffmpeg", "-y", "-loglevel", "error", "-i", input.to_string_lossy().as_ref(),
                                 "-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le", conv.to_string_lossy().as_ref()]
            .iter().map(|s| s.to_string()).collect();
        match run(&argv, 60_000, None) {
            Ok(r) if r.exit == Some(0) => conv,
            Ok(r) => fail(json, EXIT_RUNTIME, &format!("ffmpeg resample failed: {}", r.stderr.lines().last().unwrap_or("")), "run ffmpeg by hand on the file"),
            Err(e) => fail(json, EXIT_RUNTIME, &format!("could not start ffmpeg: {e}"), "check ffmpeg installation"),
        }
    } else {
        input.clone()
    };

    let argv: Vec<String> = [
        bin.to_string_lossy().as_ref(),
        "-m", model.to_string_lossy().as_ref(),
        "-f", wav16.to_string_lossy().as_ref(),
        "-np", "-nt", "-t", "10",
    ].iter().map(|s| s.to_string()).collect();
    let ran = match run(&argv, 300_000, None) {
        Ok(r) => r,
        Err(e) => fail(json, EXIT_RUNTIME, &format!("could not start whisper-cli: {e}"), "check the bin path in voice-models.json"),
    };
    if ran.exit != Some(0) {
        let tail: String = ran.stderr.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" · ");
        fail(json, EXIT_RUNTIME, &format!("whisper failed (exit {:?}): {tail}", ran.exit), "run the same command by hand for the full stderr");
    }
    let text = ran.stdout.split_whitespace().collect::<Vec<_>>().join(" ").trim().to_string();
    let rtf = if audio_seconds > 0.0 { (ran.elapsed_ms as f64 / 1000.0) / audio_seconds } else { 0.0 };

    let payload = json!({
        "text": text,
        "ear": alias,
        "audio": input.to_string_lossy(),
        "recorded": recorded,
        "seconds": (audio_seconds * 100.0).round() / 100.0,
        "elapsed_ms": ran.elapsed_ms,
        "rtf": (rtf * 1000.0).round() / 1000.0,
    });
    if json {
        emit(envelope("ok", payload), EXIT_OK);
    }
    println!("{text}");
    eprintln!("  ({}s audio · {}ms · rtf {:.2} · ear {alias})", audio_seconds.round(), ran.elapsed_ms, rtf);
    std::process::exit(EXIT_OK);
}

fn cmd_schema() -> ! {
    emit(
        envelope("ok", json!({
            "convention": "~/Dev/ClaudeWorkspace/AGENT-CLI-STANDARD.md (envelope, fix-bearing errors, exit 0/2/3/4, isatty)",
            "exit_codes": { "0": "ok", "2": "unavailable — engine or model missing; fix names the cure", "3": "bad arguments", "4": "runtime failure" },
            "registry": "~/Nexus/💻HomePC/🧰LocalModels/voice-models.json (env INTERCOM_REGISTRY overrides); engines/voices/ears are data",
            "verbs": {
                "say": { "args": ["TEXT | -", "--voice ALIAS", "--out FILE.wav", "--play"],
                          "output": { "wav": "path", "sidecar": "path (<out>.wav.json — text/voice/seconds/elapsed)", "voice": "alias", "seconds": "f64", "elapsed_ms": "u64", "played": "bool", "additionalProperties": false },
                          "notes": "one-shot piper subprocess, CPU — safe to run while the GPU paints" },
                "hear": { "args": ["FILE.wav | --mic SECONDS", "--ear ALIAS"],
                           "output": { "text": "string", "ear": "alias", "audio": "path", "recorded": "bool", "seconds": "f64", "elapsed_ms": "u64", "rtf": "f64 (elapsed/audio; <1 = faster than realtime)", "additionalProperties": false },
                           "notes": "any rate/channels accepted — ffmpeg resamples to 16 kHz mono when needed" },
                "voices": { "args": [], "output": { "voices": "[{alias, file, present, default, register}]" } },
                "ears": { "args": [], "output": { "ears": "[{alias, file, present, default, register}]" } },
                "status": { "args": [], "output": { "tts_ready": "bool", "stt_ready": "bool", "tts": "{engine, bin, bin_present, voices_installed}", "stt": "{…}", "play": "{pw_play, pw_record, ffmpeg}" }, "exit": "0 when both organs ready, else 2" },
                "schema": { "args": [], "output": "this object" }
            },
            "pipelines": {
                "llm_to_mouth": "wisp --json \"one sentence about dawn\" | jq -r .answer | intercom say - --play",
                "mic_to_llm": "intercom hear --mic 6 --json | jq -r .text | wisp",
                "bridge_to_mouth": "curl -s 127.0.0.1:8109/v1/chat/completions -d '{…}' | jq -r '.choices[0].message.content' | intercom say -",
                "roundtrip_selftest": "intercom say \"check\" --json | jq -r .wav | xargs intercom hear"
            }
        })),
        EXIT_OK,
    );
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let json_flag = args.iter().any(|a| a == "--json");
    args.retain(|a| a != "--json");
    let json = json_flag || !stdout_is_tty();

    let Some(verb) = args.first().cloned() else {
        println!("{HELP}");
        std::process::exit(EXIT_OK);
    };
    let rest = &args[1..];

    match verb.as_str() {
        "-h" | "--help" | "help" => {
            println!("{HELP}");
            std::process::exit(EXIT_OK);
        }
        "-V" | "--version" | "version" => {
            if json {
                emit(envelope("ok", json!({})), EXIT_OK);
            }
            println!("intercom {VERSION}");
            std::process::exit(EXIT_OK);
        }
        "schema" => cmd_schema(),
        _ => {}
    }

    let reg = load_registry(json);
    match verb.as_str() {
        "status" => cmd_status(&reg, json),
        "voices" => list_models(&reg, &reg.voices, "voices", json),
        "ears" => list_models(&reg, &reg.ears, "ears", json),
        "say" => cmd_say(&reg, rest, json),
        "hear" => cmd_hear(&reg, rest, json),
        other => fail(
            json,
            EXIT_BAD_ARGS,
            &format!("unknown verb `{other}`"),
            "intercom --help names every verb; `intercom schema` is the full contract",
        ),
    }
}
