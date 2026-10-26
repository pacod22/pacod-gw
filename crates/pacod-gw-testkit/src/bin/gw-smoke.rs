//! `gw-smoke`: the smoke test against a running gateway. See `pacod_gw_testkit::smoke`.
//!
//! Provider keys for the live profile and for recording are read from the environment
//! (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `ZAI_API_KEY`;
//! for Vertex AI `VERTEX_PROJECT`, `VERTEX_LOCATION` and `VERTEX_CREDENTIALS_FILE`, the path of a
//! service-account key file) and never taken as arguments, so that they do not show up in a
//! process list or a shell history. Nothing this tool prints contains them.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use pacod_gw_testkit::google::{self, ServiceAccount};
use pacod_gw_testkit::lite::{LiteOptions, LiteSmoke};
use pacod_gw_testkit::record::{Models, Recorder, Target, VertexSite};
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
    /// How to sign in: the core's own login, or pacod-id (the gateway runs with PGW_AUTH=oidc).
    #[arg(long, env = "SMOKE_AUTH", value_enum, default_value = "local", global = true)]
    auth: pacod_gw_testkit::oidc::Auth,
    /// pacod-id's issuer, with --auth oidc.
    #[arg(long, env = "SMOKE_ID_ISSUER", default_value = "http://127.0.0.1:13070", global = true)]
    id_issuer: String,
    /// Also wait for the calls to appear in the request log (needs the analytics service).
    #[arg(long, global = true)]
    events: bool,
    #[command(subcommand)]
    command: Command,
}

// Parsed once at start: the size of the largest subcommand's options costs nothing.
#[allow(clippy::large_enum_variant)]
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
        /// Only these providers: deepseek, glm, gemini-openai, gemini, vertex. Default: all.
        #[arg(long = "provider")]
        providers: Vec<String>,
        /// Where the fixtures go, one directory per provider.
        #[arg(long, default_value = "crates/pacod-gw-testkit/fixtures")]
        out: PathBuf,
        /// Only these cases (repeatable), for example `--case embed`. Default: all.
        #[arg(long = "case")]
        cases: Vec<String>,
    },
    /// Measures how long the core takes an access token after pacod-id ended its session
    /// (the target: 2 s or less). Needs a stack with PGW_AUTH=oidc; implies --auth oidc.
    Revocation {
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..))]
        samples: u16,
        /// pacod-id's internal listener, where a session is ended.
        #[arg(long, env = "SMOKE_ID_INTERNAL_URL", default_value = "http://127.0.0.1:18071")]
        id_internal: String,
        /// A service client that may end any session: the core's.
        #[arg(long, env = "SMOKE_ID_SERVICE", default_value = "pacod-gw-core")]
        service: String,
        #[arg(
            long,
            env = "SMOKE_ID_SERVICE_SECRET",
            default_value = "dev-only-core-client-secret-0123456789",
            hide_env_values = true
        )]
        service_secret: String,
    },
    /// The minimal gateway (pacod-gw-lite), which has no management API: calls what its
    /// configuration offers. The defaults are those of deploy/lite/docker-compose.dev.yml.
    /// Prints each call's request id, to be found in the gateway's log. An empty name skips
    /// its checks.
    Lite {
        #[arg(long, env = "SMOKE_LITE_URL", default_value = "http://127.0.0.1:18280")]
        url: String,
        #[arg(
            long,
            env = "SMOKE_LITE_KEY",
            default_value = "pgw_dev_DevelopmentOnlyKey0123456789",
            hide_env_values = true
        )]
        key: String,
        /// A key limited to --limited-rpm requests a minute.
        #[arg(
            long,
            env = "SMOKE_LITE_LIMITED_KEY",
            default_value = "pgw_limited_DevelopmentOnlyKey012345",
            hide_env_values = true
        )]
        limited_key: String,
        #[arg(long, default_value_t = 3)]
        limited_rpm: u32,
        /// The mock provider as this tool reaches it, to script a failover of the pool.
        #[arg(long, env = "SMOKE_LITE_MOCK_CONTROL_URL", default_value = "http://127.0.0.1:19280")]
        mock_control_url: String,
        #[arg(long, default_value = "openai/gpt-dev")]
        openai_model: String,
        #[arg(long, default_value = "anthropic/claude-dev")]
        anthropic_model: String,
        /// Without its '@'.
        #[arg(long, default_value = "fast")]
        pool: String,
        #[arg(long, default_value = "openai/text-embedding-dev")]
        embedding_model: String,
        #[arg(long, default_value = "openai/tts-dev")]
        speech_model: String,
        /// A decision model, asked through `/v1/systemone`.
        #[arg(long, default_value = "typesafe/jev-1.13.0")]
        decision_model: String,
        /// An image model, asked for one image.
        #[arg(long, default_value = "openai/gpt-image-dev")]
        image_model: String,
        /// A transcription model, given a second of audio.
        #[arg(long, default_value = "openai/whisper-dev")]
        transcription_model: String,
        /// A rerank model, given two documents.
        #[arg(long, default_value = "cohere/rerank-dev")]
        rerank_model: String,
        /// A realtime model, asked for one response in a WebSocket session.
        #[arg(long, default_value = "openai/gpt-realtime-dev")]
        realtime_model: String,
        #[arg(long, default_value = "openai")]
        proxy_provider: String,
        /// The model id the proxied provider knows.
        #[arg(long, default_value = "gpt-dev")]
        proxy_model: String,
    },
    /// Counts the lines of a log file that contain one of the provider keys in the
    /// environment, a private key or a Google access token. Exits with 1 when there is any.
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
        typesafe: provider_key("TYPESAFE_API_KEY"),
    };

    if let Command::Revocation { samples, id_internal, service, service_secret } = &args.command {
        let client = pacod_gw_testkit::oidc::Auth::Oidc.client(&args.id_issuer).expect("an oidc client");
        let measured = pacod_gw_testkit::oidc::measure_revocation(
            client,
            &args.email,
            &args.password,
            args.admin.trim_end_matches('/'),
            id_internal,
            (service, service_secret),
            usize::from(*samples),
        )
        .await;
        return match measured {
            Ok(d) if d.is_empty() => {
                eprintln!("gw-smoke: no revocation was measured");
                ExitCode::FAILURE
            }
            Ok(mut d) => {
                d.sort();
                let ms = |i: usize| d[i.min(d.len() - 1)].as_secs_f64() * 1000.0;
                let (p50, p95, max) = (ms(d.len() / 2), ms(d.len() * 95 / 100), ms(d.len() - 1));
                println!("revocation: {} samples, p50 {p50:.0} ms, p95 {p95:.0} ms, max {max:.0} ms", d.len());
                if max <= 2000.0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }
            Err(e) => {
                eprintln!("gw-smoke: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
    if let Command::Lite {
        url,
        key,
        limited_key,
        limited_rpm,
        mock_control_url,
        openai_model,
        anthropic_model,
        pool,
        embedding_model,
        speech_model,
        decision_model,
        image_model,
        transcription_model,
        rerank_model,
        realtime_model,
        proxy_provider,
        proxy_model,
    } = &args.command
    {
        let opts = LiteOptions {
            data: url.clone(),
            key: key.clone(),
            limited_key: limited_key.clone(),
            limited_rpm: *limited_rpm,
            mock_control: mock_control_url.clone(),
            openai_model: openai_model.clone(),
            anthropic_model: anthropic_model.clone(),
            pool: pool.clone(),
            embedding_model: embedding_model.clone(),
            speech_model: speech_model.clone(),
            decision_model: decision_model.clone(),
            image_model: image_model.clone(),
            transcription_model: transcription_model.clone(),
            rerank_model: rerank_model.clone(),
            realtime_model: realtime_model.clone(),
            proxy_provider: proxy_provider.clone(),
            proxy_model: proxy_model.clone(),
        };
        let mut smoke = match LiteSmoke::new(opts) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("gw-smoke: {e:#}");
                return ExitCode::FAILURE;
            }
        };
        smoke.run().await;
        smoke.print();
        return if smoke.failed() == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }
    if let Command::Record { providers, out, cases } = &args.command {
        return record(&keys, providers, out.clone(), cases.clone()).await;
    }
    if let Command::ScanLogs { file } = &args.command {
        let mut secrets = keys.all();
        // The service account's key id, when a key file is configured.
        if let Some(account) = vertex_account() {
            secrets.push(account.private_key_id);
        }
        return match std::fs::read_to_string(file) {
            Ok(text) => {
                let n = Smoke::count_leaks(&text, &secrets);
                // Whatever key file or token it came from: a private key or a Google access
                // token never belongs in a log.
                let marked = text.lines().filter(|l| l.contains("PRIVATE KEY") || l.contains("ya29.")).count();
                println!(
                    "{} of {} log lines contain a provider key, {} a private key or Google access token",
                    n,
                    text.lines().count(),
                    marked
                );
                if n + marked == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
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
        oidc: args.auth.client(&args.id_issuer),
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
        Command::Record { .. } | Command::ScanLogs { .. } | Command::Revocation { .. } | Command::Lite { .. } => {
            unreachable!("handled above")
        }
    };
    smoke.print();
    if let Err(e) = run {
        eprintln!("gw-smoke: {}", smoke.redact(&format!("{e:#}")));
        return ExitCode::FAILURE;
    }
    if smoke.failed() == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

async fn record(keys: &LiveKeys, wanted: &[String], out: PathBuf, cases: Vec<String>) -> ExitCode {
    let mut targets = Vec::new();
    for name in wanted {
        match Target::parse(name) {
            Some(t) => targets.push(t),
            None => {
                eprintln!("gw-smoke: unknown provider \"{name}\"; one of deepseek, glm, gemini-openai, gemini, vertex");
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
    recorder.only = cases;
    let mut failed = false;
    for target in targets {
        let key = if target == Target::Vertex {
            match vertex_token(&mut recorder).await {
                Ok(Some(token)) => token,
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("gw-smoke: vertex: {e:#}");
                    failed = true;
                    continue;
                }
            }
        } else {
            let Some(key) = provider_key(target.key_var()) else {
                println!("skip  {:<14} {} is not set", target.name(), target.key_var());
                continue;
            };
            key
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

/// The service account of `VERTEX_CREDENTIALS_FILE`, when it is set and readable.
fn vertex_account() -> Option<ServiceAccount> {
    let path = provider_key("VERTEX_CREDENTIALS_FILE")?;
    ServiceAccount::parse(&std::fs::read_to_string(path).ok()?).ok()
}

/// Sets up the recorder for Vertex and mints a token, or says what is missing. The key file's
/// contents are never printed.
async fn vertex_token(recorder: &mut Recorder) -> anyhow::Result<Option<String>> {
    let names = ["VERTEX_PROJECT", "VERTEX_LOCATION", "VERTEX_CREDENTIALS_FILE"];
    let missing: Vec<&str> = names.iter().copied().filter(|n| provider_key(n).is_none()).collect();
    if !missing.is_empty() {
        println!("skip  {:<14} {} not set", "vertex", missing.join(", "));
        return Ok(None);
    }
    let path = provider_key("VERTEX_CREDENTIALS_FILE").unwrap_or_default();
    let text = std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("the key file cannot be read: {e}"))?;
    let account = ServiceAccount::parse(&text)?;
    let token = google::mint(recorder.http(), &account, google::TOKEN_URL).await?;
    recorder.vertex = Some(VertexSite {
        project: provider_key("VERTEX_PROJECT").unwrap_or_default(),
        location: provider_key("VERTEX_LOCATION").unwrap_or_default(),
        client_email: account.client_email.clone(),
        secrets: vec![account.private_key_id.clone()],
    });
    Ok(Some(token))
}
