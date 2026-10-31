//! `amux internal catalogue <provider>`: runs the provider just long enough to ask
//! what it offers and whether it is signed in, writes the answer and exits.
//! The daemon starts it when it needs a host's copy, in the folder the copy
//! is kept in, with the environment it starts agents' providers with.
//! Neither question spends the account's usage.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use claude_protocol::stream::init::AccountInfo;
use claude_protocol::stream::{self as claude_stream};
use codex_protocol::client::{
    AccountReadParams, Capabilities, ClientInfo, InitializeParams, ModelListParams,
    SkillsListParams,
};
use codex_protocol::server::{AccountReadResponse, ModelListResponse, SkillsListResponse};
use codex_protocol::{ClientMessage, ClientNotification, ClientRequest, RequestId, ServerMessage};
use prost::Message as _;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use wire::{Catalogue, Provider, ProviderOffer};

const INITIALIZE: &str = "amux-catalogue";

/// The provider a word on the command line names: the `Provider` value's
/// own name, lowercase, without the enum's prefix, as the daemon writes it.
pub fn provider_named(word: &str) -> Option<Provider> {
    Provider::from_str_name(&format!("PROVIDER_{}", word.to_ascii_uppercase()))
        .filter(|provider| *provider != Provider::Unspecified)
}

/// Asks `provider`, run as `command`, and writes a `ProviderOffer` to
/// `out`; the exit code says whether it could.
pub fn main(provider: &str, command: &str, out: &Path) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("amux catalogue: {error}");
            return 1;
        }
    };
    let asked = runtime.block_on(async {
        match provider_named(provider) {
            Some(Provider::Claude) => ask_claude(command).await,
            Some(Provider::Codex) => ask_codex(command).await,
            Some(Provider::Unspecified) | None => Err(format!("no provider named {provider:?}")),
        }
    });
    let written = asked.and_then(|(catalogue, signed_in)| {
        let offer = ProviderOffer {
            catalogue: Some(catalogue),
            signed_in,
            ..ProviderOffer::default()
        };
        std::fs::write(out, offer.encode_to_vec())
            .map_err(|error| format!("writing {}: {error}", out.display()))
    });
    match written {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("amux catalogue: {error}");
            1
        }
    }
}

/// Headless Claude's initialize answer lists its models and commands and
/// names the account. Claude starts no tool server and runs no hook for it,
/// except where the organization manages its MCP servers: Claude there
/// refuses to be told to start none, and starts the organization's.
async fn ask_claude(command: &str) -> Result<(Catalogue, bool), String> {
    let managed = claude::launch::managed_mcp(
        std::env::var_os(claude::launch::MANAGED_SETTINGS_PATH_ENV)
            .as_deref()
            .map(Path::new),
    );
    let only_named = [
        "--strict-mcp-config",
        "--mcp-config",
        r#"{"mcpServers":{}}"#,
    ];
    let mut child = tokio::process::Command::new(command)
        .args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--settings",
            r#"{"disableAllHooks":true}"#,
        ])
        .args(if managed { &[][..] } else { &only_named[..] })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("starting {command}: {error}"))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    let initialize = claude_stream::ControlRequest::new(
        INITIALIZE,
        claude_stream::ControlRequestBody::Initialize(Default::default()),
    );
    let mut line = claude_stream::encode(&claude_stream::Input::ControlRequest(initialize));
    line.push(b'\n');
    stdin
        .write_all(&line)
        .await
        .map_err(|error| format!("writing to Claude: {error}"))?;
    let mut lines = BufReader::new(stdout).lines();
    let answer = loop {
        let Some(line) = lines
            .next_line()
            .await
            .map_err(|error| format!("reading Claude: {error}"))?
        else {
            return Err("Claude ended before it answered".to_owned());
        };
        if let Ok(claude_stream::Output::ControlResponse(response)) =
            claude_stream::decode(line.as_bytes())
            && response.request_id() == INITIALIZE
        {
            break response;
        }
    };
    drop(stdin);
    let _ = child.start_kill();
    let initialized = match answer.result::<claude_stream::InitializationResult>() {
        Some(Ok(initialized)) => initialized,
        Some(Err(error)) => return Err(format!("Claude's initialize answer: {error}")),
        None => {
            return Err(format!(
                "Claude refused initialize: {}",
                answer.response.error.unwrap_or_default()
            ));
        }
    };
    Ok((
        interpret::claude_sdk::host_catalogue(&initialized),
        claude_signed_in(&initialized.account),
    ))
}

/// Signed out, Claude names no credential, only its backend; one other than
/// Anthropic's own takes no sign-in.
fn claude_signed_in(account: &AccountInfo) -> bool {
    account
        .api_provider
        .as_deref()
        .is_some_and(|backend| backend != "firstParty")
        || account.email.is_some()
        || account.subscription_type.is_some()
        || account.token_source.is_some()
        || account.api_key_source.is_some()
}

/// Codex's app server lists its models (in pages) and skills before any
/// thread, and reads the account.
async fn ask_codex(command: &str) -> Result<(Catalogue, bool), String> {
    let listen = codex::host::Listen::Stdio;
    let mut server = tokio::process::Command::new(command);
    server.stderr(Stdio::null()).kill_on_drop(true);
    let mut child = codex::host::spawn_server(server, &listen)
        .map_err(|error| format!("starting {command}: {error}"))?;
    let (sender, receiver) = codex::host::Connection::connect(&listen, &mut child, Duration::ZERO)
        .await
        .map_err(|error| format!("starting {command}: {error}"))?
        .into_split();
    let mut codex = CodexProbe {
        sender,
        receiver,
        asked: 0,
    };
    let asked = async {
        let _: serde_json::Value = codex
            .ask(ClientRequest::Initialize(InitializeParams {
                client_info: ClientInfo {
                    name: "amux".into(),
                    title: None,
                    version: crate::VERSION.into(),
                    extra: Default::default(),
                },
                capabilities: Some(Capabilities {
                    experimental_api: Some(true),
                    opt_out_notification_methods: Vec::new(),
                    extra: Default::default(),
                }),
                extra: Default::default(),
            }))
            .await?;
        codex
            .send(ClientMessage::notification(
                ClientNotification::Initialized(()),
            ))
            .await?;
        let mut models = Vec::new();
        let mut cursor = None;
        loop {
            let page: ModelListResponse = codex
                .ask(ClientRequest::ModelList(ModelListParams {
                    cursor: cursor.take(),
                    ..Default::default()
                }))
                .await?;
            let more = !page.data.is_empty();
            models.extend(page.data);
            match page.next_cursor.filter(|next| more && !next.is_empty()) {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let skills: SkillsListResponse = codex
            .ask(ClientRequest::SkillsList(SkillsListParams::default()))
            .await?;
        let account: AccountReadResponse = codex
            .ask(ClientRequest::AccountRead(AccountReadParams {
                refresh_token: Some(false),
                ..Default::default()
            }))
            .await?;
        Ok::<_, String>((models, skills, account))
    }
    .await;
    // The end of its input ends a stdio server; one that lingers is killed.
    let _ = codex.sender.close().await;
    if tokio::time::timeout(CODEX_EXIT_WAIT, child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
    }
    let (models, skills, account) = asked.map_err(|error| format!("asking Codex: {error}"))?;
    let signed_in = account.account.is_some() || !account.requires_openai_auth;
    Ok((interpret::codex::host_catalogue(&models, skills), signed_in))
}

/// How long Codex has to exit once the probe has closed its input.
const CODEX_EXIT_WAIT: Duration = Duration::from_secs(2);

/// The probe's one connection to Codex's server, asking one thing at a time.
struct CodexProbe {
    sender: codex::host::Sender,
    receiver: codex::host::Receiver,
    asked: i64,
}

impl CodexProbe {
    async fn send(&mut self, message: ClientMessage) -> Result<(), String> {
        self.sender
            .send(&codex_protocol::encode(&message))
            .await
            .map_err(|error| format!("writing to Codex: {error}"))
    }

    /// Sends `request` and reads its answer as `R`; whatever else the
    /// server says meanwhile is passed over.
    async fn ask<R: serde::de::DeserializeOwned>(
        &mut self,
        request: ClientRequest,
    ) -> Result<R, String> {
        self.asked += 1;
        let id = RequestId::Integer(self.asked);
        self.send(ClientMessage::request(id.clone(), request))
            .await?;
        loop {
            let line = self
                .receiver
                .next()
                .await
                .ok_or("Codex ended before it answered")?
                .map_err(|error| format!("reading Codex: {error}"))?;
            if let Ok(ServerMessage::Response {
                id: answered,
                result,
                ..
            }) = codex_protocol::decode(&line)
                && answered == id
            {
                let result = result.map_err(|error| error.message)?;
                return codex_protocol::result(&result).map_err(|error| error.to_string());
            }
        }
    }
}
