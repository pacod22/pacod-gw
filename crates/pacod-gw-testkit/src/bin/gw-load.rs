//! `gw-load`: the load generator. It sets up a key and a model at the mock provider, sends
//! the same series of calls to the mock directly and through the gateway, and reports what
//! the gateway added. See `pacod_gw_testkit::load`.

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use pacod_gw_testkit::load::{Outcome, Plan, run};
use pacod_gw_testkit::smoke::{Options, Smoke};
use serde_json::json;

#[derive(Clone, Copy, ValueEnum)]
enum Profile {
    /// `/proxy/{provider}/v1/chat/completions`
    Proxy,
    /// `/v1/chat/completions`
    Managed,
}

#[derive(Parser)]
#[command(name = "gw-load", version)]
struct Args {
    #[arg(long, value_enum, default_value = "proxy")]
    profile: Profile,
    #[arg(long, env = "SMOKE_ADMIN_URL", default_value = "http://localhost:18081")]
    admin: String,
    #[arg(long, env = "SMOKE_DATA_URL", default_value = "http://localhost:18080")]
    data: String,
    #[arg(long, env = "PGW_ADMIN_EMAIL", default_value = "admin@example.com")]
    email: String,
    #[arg(long, env = "PGW_ADMIN_PASSWORD", default_value = "pacod-gw-dev", hide_env_values = true)]
    password: String,
    /// How to sign in: the core's own login, or pacod-id (the gateway runs with PGW_AUTH=oidc).
    #[arg(long, env = "SMOKE_AUTH", value_enum, default_value = "local")]
    auth: pacod_gw_testkit::oidc::Auth,
    /// pacod-id's issuer, with --auth oidc.
    #[arg(long, env = "SMOKE_ID_ISSUER", default_value = "http://127.0.0.1:13070")]
    id_issuer: String,
    /// The mock provider as the gateway reaches it.
    #[arg(long, env = "SMOKE_MOCK_URL", default_value = "http://gw-mock:9000")]
    mock_url: String,
    /// The mock provider as this tool reaches it, for the direct comparison.
    #[arg(long, env = "LOAD_MOCK_DIRECT_URL", default_value = "http://localhost:19000")]
    mock_direct: String,
    #[arg(long, default_value_t = 2000)]
    requests: usize,
    #[arg(long, default_value_t = 100)]
    concurrency: usize,
    /// Send non-streamed calls instead of streams.
    #[arg(long)]
    no_stream: bool,
    /// Words in the prompt; the mock answers with as many stream chunks.
    #[arg(long, default_value_t = 60)]
    words: usize,
}

fn line(name: &str, o: &Outcome) {
    println!(
        "{name:<10} {:>6} ok {:>4} failed {:>8.0} req/s   first byte p50 {:>7.2} p95 {:>7.2} p99 {:>7.2} ms   total p50 {:>7.2} p99 {:>7.2} ms",
        o.ok,
        o.failed,
        o.per_second(),
        o.first_byte_ms.p50,
        o.first_byte_ms.p95,
        o.first_byte_ms.p99,
        o.total_ms.p50,
        o.total_ms.p99
    );
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let options = Options {
        admin: args.admin.trim_end_matches('/').to_owned(),
        data: args.data.trim_end_matches('/').to_owned(),
        email: args.email,
        password: args.password,
        oidc: args.auth.client(&args.id_issuer),
        mock_url: args.mock_url,
        mock_control: String::new(),
        check_events: false,
    };
    let data = options.data.clone();
    let scene = match async { Smoke::connect(options).await?.load_scene().await }.await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gw-load: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    let stream = !args.no_stream;
    let prompt = vec!["word"; args.words.max(1)].join(" ");
    let body = |model: &str| {
        let mut b = json!({ "model": model, "stream": stream, "messages": [{ "role": "user", "content": prompt }] });
        if stream {
            b["stream_options"] = json!({ "include_usage": true });
        }
        b
    };
    let http = match reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .pool_max_idle_per_host(args.concurrency)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gw-load: {e}");
            return ExitCode::FAILURE;
        }
    };
    let direct = Plan {
        url: format!("{}/v1/chat/completions", args.mock_direct.trim_end_matches('/')),
        key: None,
        body: body(&scene.upstream),
        requests: args.requests,
        concurrency: args.concurrency,
    };
    let through = match args.profile {
        Profile::Proxy => Plan {
            url: format!("{data}/proxy/{}/v1/chat/completions", scene.provider_name),
            key: Some(scene.key.clone()),
            body: body(&scene.upstream),
            ..direct.clone()
        },
        Profile::Managed => Plan {
            url: format!("{data}/v1/chat/completions"),
            key: Some(scene.key.clone()),
            body: body(&scene.model),
            ..direct.clone()
        },
    };

    println!(
        "{} calls, {} at a time, {}, {} words",
        args.requests,
        args.concurrency,
        if stream { "streamed" } else { "not streamed" },
        args.words
    );
    // A short warm-up fills the connection pools on every hop.
    let warm = |p: &Plan| Plan { requests: args.concurrency.min(p.requests), ..p.clone() };
    run(&http, &warm(&direct)).await;
    run(&http, &warm(&through)).await;

    let d = run(&http, &direct).await;
    let g = run(&http, &through).await;
    line("direct", &d);
    line("gateway", &g);
    println!(
        "added by the gateway, seen from outside: first byte p50 {:+.2} ms, p99 {:+.2} ms",
        g.first_byte_ms.p50 - d.first_byte_ms.p50,
        g.first_byte_ms.p99 - d.first_byte_ms.p99
    );
    if let Some(own) = &g.gateway_delay_ms {
        println!(
            "the gateway's own figure (x-gw-delay-ms):  p50 {:.3} ms, p95 {:.3} ms, p99 {:.3} ms, max {:.3} ms",
            own.p50, own.p95, own.p99, own.max
        );
    }
    if g.failed > 0 || d.failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
