//! `gw-smoke`: the smoke test against a running gateway. See `pacod_gw_testkit::smoke`.
//!
//! Provider keys for the live profile and for recording are read from the environment
//! (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `ZAI_API_KEY`)
//! and never taken as arguments, so that they do not show up in a process list or a shell
//! history. Nothing this tool prints contains them.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use pacod_gw_testkit::record::{Models, Recorder, Target};
use pacod_gw_testkit::smoke::{LiveKeys, LiveModels, Options, Smoke};

#[derive(Parser)]
#[command(name = "gw-smoke", version)]
struct Args {
    /// The gateway's management API.
    #[arg(long, env = "SMOKE_ADMIN_URL", default_value = "http://localhost:18081", global = true)]
    admin: String,
    /// The gateway's data plane.
    #[arg(long, env = "SMOKE_DATA_URL", default_value = "http://localhost:18080", global = true)]
    data: String,
    #[arg(long, env = "PGW_ADMIN_EMAIL", default_value = "admin@example.com", global = true)]
    email: String,
    #[arg(long, env = "PGW_ADMIN_PASSWORD", default_value = "pacod-gw-dev", hide_env_values = true, global = true)]
    password: String,
    /// Also wait for the calls to appear in the request log (needs the analytics service).
    #[arg(long, global = true)]
    events: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The full flow against the scripted mock provider.
    Mock {
        /// The mock provider as the gateway reaches it.
        #[arg(long, env = "SMOKE_MOCK_URL", default_value = "http://gw-mock:9000")]
        mock_url: String,
        /// The mock provider as this tool reaches it, to script a failure. Empty: skipped.
        #[arg(long, env = "SMOKE_MOCK_CONTROL_URL", default_value = "http://127.0.0.1:19000")]
        mock_control_url: String,
    },
    /// A few tiny calls per provider with the real keys in the environment.
    Live {
        /// The OpenAI model to call. Default: a small one from the provider's list.
        #[arg(long, env = "SMOKE_OPENAI_MODEL")]
        openai_model: Option<String>,
        /// The Anthropic model to call. Default: a Haiku model from the provider's list.
        #[arg(long, env = "SMOKE_ANTHROPIC_MODEL")]
        anthropic_model: Option<String>,
        /// The Gemini model to call. Default: `gemini-3.8-flash`.
        #[arg(long, env = "SMOKE_GEMINI_MODEL")]
        gemini_model: Option<String>,
        /// The DeepSeek model to call. Default: `deepseek-flash`.
        #[arg(long, env = "SMOKE_DEEPSEEK_MODEL")]
        deepseek_model: Option<String>,
        /// The GLM model to call. Default: `glm-4.7`.
        #[arg(long, env = "SMOKE_GLM_MODEL")]
        glm_model: Option<String>,
    },
    /// Calls providers directly (not through the gateway) and writes what they answer as
    /// scrubbed fixtures. A provider whose key is not set is skipped.
    Record {
        /// Only these providers: deepseek, glm, gemini-openai, gemini. Default: all.
        #[arg(long = "provider")]
        providers: Vec<String>,
        /// Where the fixtures go, one directory per provider.
        #[arg(long, default_value = "crates/pacod-gw-testkit/fixtures")]
        out: PathBuf,
    },
    /// Counts the lines of a log file that contain one of the provider keys in the
    /// environment. Exits with 1 when there is any.
    ScanLogs { file: PathBuf },
}

fn provider_key(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let keys = LiveKeys {
        openai: provider_key("OPENAI_API_KEY"),
        anthropic: provider_key("ANTHROPIC_API_KEY"),
        gemini: provider_key("GEMINI_API_KEY"),
        deepseek: provider_key("DEEPSEEK_API_KEY"),
        glm: provider_key("ZAI_API_KEY"),
    };

    if let Command::Record { providers, out } = &args.command {
        return record(&keys, providers, out.clone()).await;
    }
    if let Command::ScanLogs { file } = &args.command {
        let secrets = keys.all();
        if secrets.is_empty() {
            println!("skip  no provider key in the environment; nothing to look for");
            return ExitCode::SUCCESS;
        }
        return match std::fs::read_to_string(file) {
            Ok(text) => {
                let n = Smoke::count_leaks(&text, &secrets);
                println!("{} of {} log lines contain a provider key", n, text.lines().count());
                if n == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }
            Err(e) => {
                eprintln!("could not read {}: {e}", file.display());
                ExitCode::FAILURE
            }
        };
    }

    let (mock_url, mock_control) = match &args.command {
        Command::Mock { mock_url, mock_control_url } => (mock_url.clone(), mock_control_url.clone()),
        _ => (String::new(), String::new()),
    };
    let options = Options {
        admin: args.admin.trim_end_matches('/').to_owned(),
        data: args.data.trim_end_matches('/').to_owned(),
        email: args.email,
        password: args.password,
        mock_url,
        mock_control,
        check_events: args.events,
    };
    let mut smoke = match Smoke::connect(options).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gw-smoke: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let run = match args.command {
        Command::Mock { .. } => smoke.run_mock().await,
        Command::Live { openai_model, anthropic_model, gemini_model, deepseek_model, glm_model } => {
            let models = LiveModels {
                openai: openai_model,
                anthropic: anthropic_model,
                gemini: gemini_model,
                deepseek: deepseek_model,
                glm: glm_model,
            };
            smoke.run_live(keys, models).await
        }
        Command::Record { .. } | Command::ScanLogs { .. } => unreachable!("handled above"),
    };
    smoke.print();
    if let Err(e) = run {
        eprintln!("gw-smoke: {}", smoke.redact(&format!("{e:#}")));
        return ExitCode::FAILURE;
    }
    if smoke.failed() == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

async fn record(keys: &LiveKeys, wanted: &[String], out: PathBuf) -> ExitCode {
    let mut targets = Vec::new();
    for name in wanted {
        match Target::parse(name) {
            Some(t) => targets.push(t),
            None => {
                eprintln!("gw-smoke: unknown provider \"{name}\"; one of deepseek, glm, gemini-openai, gemini");
                return ExitCode::FAILURE;
            }
        }
    }
    if targets.is_empty() {
        targets = Target::ALL.to_vec();
    }
    let mut recorder = match Recorder::new(out, Models::default(), keys.all()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("gw-smoke: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = false;
    for target in targets {
        let Some(key) = provider_key(target.key_var()) else {
            println!("skip  {:<14} {} is not set", target.name(), target.key_var());
            continue;
        };
        if let Err(e) = recorder.record(target, &key).await {
            // The error names the case, never the key.
            eprintln!("gw-smoke: {e:#}");
            failed = true;
        }
    }
    for line in &recorder.lines {
        println!("{line}");
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
