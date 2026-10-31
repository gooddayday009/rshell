//! Opt-in live Claude Agent SDK/MCP probe. Only synthetic terminal data; no command execution.
use rshell_core::claude_agent::{self, Event, Settings};

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--agent-mcp") {
        return claude_agent::serve_stdio();
    }
    if std::env::args().any(|a| a == "--loopback-profile") {
        return tokio::runtime::Runtime::new()?.block_on(loopback_profile());
    }
    anyhow::ensure!(
        std::env::var("RSHELL_CLAUDE_LIVE_TEST").as_deref() == Ok("1"),
        "Set RSHELL_CLAUDE_LIVE_TEST=1 to opt in to a real model request using synthetic data"
    );
    tokio::runtime::Runtime::new()?.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let host = tokio::spawn(async move {
            let mut reads = 0;
            let mut proposals = 0;
            while let Some(event) = rx.recv().await {
                match event {
                    Event::Text(_) | Event::Status(_) => {},
                    Event::ComponentRequired { reply, .. } => { let _ = reply.send(claude_agent::ComponentChoice::Cancel); },
                    Event::Tool { name, arguments, reply } => {
                        let answer = match name.as_str() {
                            "read_terminal" => { reads += 1; "Synthetic terminal: Windows PowerShell, PS C:\\Probe>" },
                            "execute_command" => {
                                anyhow::ensure!(arguments["command"].as_str() == Some("Get-Date"), "unexpected proposal");
                                proposals += 1;
                                "User rejected the proposed command. Nothing was executed. Stop calling tools and report rejection."
                            },
                            _ => anyhow::bail!("unexpected tool"),
                        };
                        let _ = reply.send(answer.into());
                    }
                }
            }
            Ok::<_, anyhow::Error>((reads, proposals))
        });
        let settings = Settings {
            sdk_executable: std::env::current_exe()?.parent().unwrap().parent().unwrap().join("agent-sdk/rshell-agent-sdk.exe").to_string_lossy().into_owned(),
            ..Default::default()
        };
        let answer = claude_agent::run(settings, "This is an integration test using only synthetic terminal data. Call read_terminal once, then propose exactly Get-Date using execute_command with purpose, impact and prerequisites. If rejected, stop and say it was not executed. Do not use any other tools.".into(), tx).await?;
        let (reads, proposals) = host.await??;
        anyhow::ensure!(reads > 0 && proposals == 1, "expected terminal read and one rejected proposal; got {reads}/{proposals}");
        println!("LIVE PROBE PASSED: reads={reads}, proposals={proposals}, executed=0\n{}", claude_agent::redact(&answer));
        Ok(())
    })
}

/// Uses the real packaged SDK against a local synthetic API, never a real provider.
async fn loopback_profile() -> anyhow::Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await?;
            let seen = seen.clone();
            // The engine can send count-token and message requests concurrently.
            tokio::spawn(async move {
                let operation = async {
                    let mut reader = tokio::io::BufReader::new(socket);
                    let mut headers = String::new();
                    loop {
                        let mut line = String::new();
                        anyhow::ensure!(reader.read_line(&mut line).await? > 0, "unexpected EOF");
                        headers.push_str(&line);
                        anyhow::ensure!(headers.len() < 65536, "headers too large");
                        if line == "\r\n" {
                            break;
                        }
                    }
                    let length: usize = headers
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| "0".into())
                        .parse()?;
                    anyhow::ensure!(length <= 1_048_576, "request too large");
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).await?;
                    let body: serde_json::Value = serde_json::from_slice(&body)?;
                    let valid = headers.starts_with("POST /selected/v1/messages")
                        && headers
                            .to_lowercase()
                            .contains("x-api-key: synthetic-selected-key")
                        && body["model"] == "claude-sonnet-custom-profile";
                    if valid {
                        seen.fetch_add(1, Ordering::SeqCst);
                    }
                    let (mime, response) = if headers.contains("/count_tokens") {
                        ("application/json", "{\"input_tokens\":10}".to_owned())
                    } else if body["stream"] == true {
                        let events = [
                            serde_json::json!({"type":"message_start","message":{"id":"msg_loopback","type":"message","role":"assistant","model":"claude-sonnet-custom-profile","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}),
                            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"PROFILE_ROUTE_OK"}}),
                            serde_json::json!({"type":"content_block_stop","index":0}),
                            serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}),
                            serde_json::json!({"type":"message_stop"}),
                        ];
                        (
                            "text/event-stream",
                            events
                                .iter()
                                .map(|e| {
                                    format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap())
                                })
                                .collect::<String>(),
                        )
                    } else {
                        ("application/json", serde_json::json!({"id":"msg_loopback","type":"message","role":"assistant","model":"claude-sonnet-custom-profile","content":[{"type":"text","text":"PROFILE_ROUTE_OK"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5}}).to_string())
                    };
                    reader.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await?;
                    Ok::<_, anyhow::Error>(())
                };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(10), operation).await;
            });
        }
        #[allow(unreachable_code)]
        Ok::<_, anyhow::Error>(())
    });
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let host = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Event::Tool { reply, .. } = event {
                let _ = reply.send("No terminal execution is authorized in this probe.".into());
            }
        }
    });
    let settings = Settings {
        sdk_executable: std::env::current_exe()?
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("agent-sdk/rshell-agent-sdk.exe")
            .to_string_lossy()
            .into_owned(),
        model: "must-be-overridden".into(),
        ..Default::default()
    };
    let profile = rshell_core::ai::AiProfile {
        api_format: rshell_core::ai::ApiFormat::Anthropic,
        base_url: format!("http://{address}/selected/v1"),
        model: "claude-sonnet-custom-profile".into(),
        ..Default::default()
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        claude_agent::run_profile(
            settings,
            profile,
            zeroize::Zeroizing::new("synthetic-selected-key".into()),
            "Respond PROFILE_ROUTE_OK. Do not call tools.".into(),
            tx,
        ),
    )
    .await;
    server.abort();
    host.abort();
    let answer = result??;
    anyhow::ensure!(answer.contains("PROFILE_ROUTE_OK"), "unexpected SDK result");
    anyhow::ensure!(
        requests.load(Ordering::SeqCst) > 0,
        "selected profile was not used"
    );
    println!(
        "PROFILE LOOPBACK PASSED: packaged SDK used selected endpoint, API key and model; executed=0"
    );
    Ok(())
}
